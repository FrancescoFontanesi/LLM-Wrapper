//! The daemon proper: carries out commands, reacts to the window, keeps the state file true.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, glib};
use llm_wrapper_protocol::{Request, Response, Status};

use crate::core::{self, Active, Model, Providers, Visibility, DEFAULT_IDLE_MINUTES};
use crate::ipc::Reply;
use crate::policy::Policy;
use crate::recovery::Recovery;
use crate::state::{Snapshot, StateFile};
use crate::transfers::Transfers;
use crate::ui::Ui;
use crate::{log, providers};

struct Mutable {
    model: Model,
    idle_minutes: u32,
    idle_timer: Option<glib::SourceId>,
}

pub struct App {
    ui: Ui,
    recovery: Rc<Recovery>,
    policy: Rc<Policy>,
    state_file: RefCell<StateFile>,
    st: RefCell<Mutable>,
    providers_file: PathBuf,
    instance: Option<String>,
    main_loop: glib::MainLoop,
}

fn rejected(message: &str) -> Reply {
    Reply {
        response: Response::with_message(Status::Rejected, message),
        then_quit: false,
    }
}

impl App {
    pub fn new(
        ui: Ui,
        providers: Providers,
        providers_file: PathBuf,
        state_file: StateFile,
        instance: Option<String>,
        main_loop: glib::MainLoop,
    ) -> Rc<Self> {
        let recovery = Recovery::attach(&ui.view);
        let transfers = Transfers::new(ui.window.clone());
        transfers.attach_downloads(ui.session());
        let policy = Policy::new(ui.window.clone(), transfers);
        policy.attach(&ui.view);
        let app = Rc::new(Self {
            ui,
            recovery,
            policy,
            state_file: RefCell::new(state_file),
            st: RefCell::new(Mutable {
                model: Model {
                    providers,
                    active: None,
                },
                idle_minutes: DEFAULT_IDLE_MINUTES,
                idle_timer: None,
            }),
            providers_file,
            instance,
            main_loop,
        });
        app.connect_signals();
        app
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.ui.window.connect_visible_notify({
            let weak = weak.clone();
            move |_| {
                if let Some(app) = weak.upgrade() {
                    app.on_visibility_changed();
                }
            }
        });

        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture); // before the page sees them
        keys.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
            Some(app) => app.on_key(key, state),
            None => glib::Propagation::Proceed,
        });
        self.ui.window.add_controller(keys);
    }

    /// Carry out a request that has been decoded and validated. The answer says what really happened.
    pub fn execute(self: &Rc<Self>, request: &Request) -> Reply {
        let plan = {
            let st = self.st.borrow();
            core::plan(&st.model, request)
        };
        let plan = match plan {
            Ok(p) => p,
            Err(message) => return rejected(&message),
        };
        if plan.quit {
            return Reply {
                response: Response::ok(),
                then_quit: true,
            };
        }

        // the one step that can fail comes first, so a failure leaves nothing half done
        if let Some(list) = &plan.providers {
            if let Err(e) = providers::save(&self.providers_file, list) {
                log::error(&format!("cannot save the provider list: {e}"));
                return Reply {
                    response: Response::with_message(
                        Status::Internal,
                        &format!("cannot save the provider list: {e}"),
                    ),
                    then_quit: false,
                };
            }
            self.st.borrow_mut().model.providers = list.clone();
        }
        if let Some(ua) = &plan.user_agent {
            self.ui.set_user_agent(ua);
        }
        if let Some(minutes) = plan.idle_minutes {
            self.st.borrow_mut().idle_minutes = minutes;
            if !self.ui.is_visible() {
                self.cancel_idle();
                self.schedule_idle();
            }
        }

        let mut switched = false;
        if let Some(active) = plan.activate {
            switched = self.activate(active);
        }
        if plan.new_chat && !switched {
            self.new_chat();
        }
        if plan.soft_clean {
            self.ui.soft_clean();
        }
        if plan.hard_clean {
            self.ui.hard_clean();
        }
        match plan.visibility {
            Visibility::Show => self.ui.show(),
            Visibility::Hide => self.ui.hide(),
            Visibility::Toggle => self.ui.toggle(),
            Visibility::Keep => {}
        }
        self.write_state();
        Reply {
            response: Response::ok(),
            then_quit: false,
        }
    }

    /// Make `active` the page on screen; returns whether a page was actually loaded.
    fn activate(&self, active: Active) -> bool {
        let changed = self.st.borrow().model.active.as_ref() != Some(&active);
        if changed {
            self.ui.set_title_for(&active.name);
            self.recovery.note_requested(&active.url);
            self.policy.set_provider(&active.url);
            self.ui.load(&active.url);
            self.st.borrow_mut().model.active = Some(active);
        }
        changed
    }

    fn new_chat(&self) {
        let url = self
            .st
            .borrow()
            .model
            .active
            .as_ref()
            .map(|a| a.url.clone());
        if let Some(url) = url {
            self.recovery.note_requested(&url);
            self.ui.load(&url);
        }
    }

    pub fn write_state(&self) {
        let st = self.st.borrow();
        let Some(active) = &st.model.active else {
            return;
        };
        let snapshot = Snapshot {
            instance: self.instance.as_deref(),
            active: &active.name,
            url: &active.url,
            visible: self.ui.is_visible(),
            providers: &st.model.providers,
        };
        if let Err(e) = self.state_file.borrow_mut().write(&snapshot) {
            log::warn(&format!("cannot write the state file: {e}"));
        }
    }

    pub fn remove_state(&self) {
        self.state_file.borrow_mut().remove();
    }

    pub fn quit(&self) {
        self.main_loop.quit();
    }

    // ---- window events -------------------------------------------------------------------------

    fn on_visibility_changed(self: &Rc<Self>) {
        if self.ui.is_visible() {
            self.cancel_idle();
            self.ui.set_trimmed(false);
            self.ui.grab_focus();
        } else {
            self.schedule_idle();
        }
        self.write_state();
    }

    fn schedule_idle(self: &Rc<Self>) {
        let mut st = self.st.borrow_mut();
        if st.idle_timer.is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        st.idle_timer = Some(glib::timeout_add_seconds_local_once(
            st.idle_minutes.saturating_mul(60),
            move || {
                let Some(app) = weak.upgrade() else { return };
                app.st.borrow_mut().idle_timer = None; // fired: nothing left to remove
                if !app.ui.is_visible() {
                    app.ui.set_trimmed(true);
                    app.ui.soft_clean();
                }
            },
        ));
    }

    fn cancel_idle(&self) {
        if let Some(timer) = self.st.borrow_mut().idle_timer.take() {
            timer.remove();
        }
    }

    /// The assistant's key bindings (the same as the legacy app's).
    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        use gdk::ModifierType as Mod;
        let mods = state & gtk4::accelerator_get_default_mod_mask();
        let is = |keys: &[gdk::Key]| keys.contains(&key);

        if key == gdk::Key::Escape && mods.is_empty() {
            self.ui.hide(); // and the page never sees the key, so it cannot close its own menus
        } else if is(&[gdk::Key::n, gdk::Key::N]) && mods == Mod::CONTROL_MASK {
            self.new_chat();
        } else if (mods.contains(Mod::CONTROL_MASK) && is(&[gdk::Key::r, gdk::Key::R]))
            || key == gdk::Key::F5
        {
            self.ui.reload(mods.contains(Mod::SHIFT_MASK));
        } else if key == gdk::Key::F12 {
            self.ui.toggle_inspector();
        } else if mods & !Mod::SHIFT_MASK == Mod::CONTROL_MASK
            && is(&[gdk::Key::plus, gdk::Key::equal, gdk::Key::KP_Add])
        {
            self.ui.zoom(Some(0.1));
        } else if mods & !Mod::SHIFT_MASK == Mod::CONTROL_MASK
            && is(&[gdk::Key::minus, gdk::Key::underscore, gdk::Key::KP_Subtract])
        {
            self.ui.zoom(Some(-0.1));
        } else if mods & !Mod::SHIFT_MASK == Mod::CONTROL_MASK
            && is(&[gdk::Key::_0, gdk::Key::KP_0])
        {
            self.ui.zoom(None);
        } else {
            return glib::Propagation::Proceed;
        }
        glib::Propagation::Stop
    }
}
