//! What the web views may do on their own: navigate, open windows, ask for permissions.
//!
//! The decisions themselves are in [`crate::nav`]; this attaches them to a view. Every view the
//! app creates — the main one and the sign-in popups — goes through [`Policy::attach`].

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::gio::{self, prelude::*};
use gtk4::{glib, prelude::*};
use webkit6::prelude::*;
use webkit6::{
    NavigationAction, NavigationPolicyDecision, NavigationType, PermissionRequest, PolicyDecision,
    PolicyDecisionType, ResponsePolicyDecision, WebView,
};

use crate::log;
use crate::nav::{self, Navigation, Outcome};
use crate::transfers::Transfers;

/// Sign-in popups open at once; a page asking for more gets none.
const MAX_POPUPS: usize = 4;
/// Pause between two hand-overs to the browser.
const BROWSER_COOLDOWN: Duration = Duration::from_millis(500);
const POPUP_WIDTH: i32 = 600;
const POPUP_HEIGHT: i32 = 720;

/// Navigation, popup and permission policy for the views of one window.
pub struct Policy {
    window: gtk4::Window,
    transfers: Rc<Transfers>,
    /// URL of the provider on screen: what "its own site" means.
    provider: RefCell<String>,
    popups: Cell<usize>,
    last_browser_launch: Cell<Option<Instant>>,
    denied: RefCell<HashSet<String>>,
}

impl Policy {
    pub fn new(window: gtk4::Window, transfers: Rc<Transfers>) -> Rc<Self> {
        Rc::new(Self {
            window,
            transfers,
            provider: RefCell::default(),
            popups: Cell::new(0),
            last_browser_launch: Cell::new(None),
            denied: RefCell::default(),
        })
    }

    /// The provider whose site the views are showing.
    pub fn set_provider(&self, url: &str) {
        url.clone_into(&mut self.provider.borrow_mut());
    }

    /// Put `view` under the policy.
    pub fn attach(self: &Rc<Self>, view: &WebView) {
        let me = self.clone();
        view.connect_decide_policy(move |_, decision, kind| me.on_decision(decision, kind));
        let me = self.clone();
        view.connect_create(move |opener, action| me.on_create(opener, action));
        let me = self.clone();
        view.connect_permission_request(move |_, request| me.deny(request));
        let me = self.clone();
        view.connect_run_file_chooser(move |_, request| {
            me.transfers.choose_upload(request);
            true
        });
    }

    fn on_decision(&self, decision: &PolicyDecision, kind: PolicyDecisionType) -> bool {
        match kind {
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                    return false;
                };
                let Some(mut action) = navigation.navigation_action() else {
                    return false;
                };
                let uri = action
                    .request()
                    .and_then(|r| r.uri())
                    .map(|u| u.to_string())
                    .unwrap_or_default();
                let new_window = kind == PolicyDecisionType::NewWindowAction;
                let request = Navigation {
                    new_window,
                    link_clicked: action.navigation_type() == NavigationType::LinkClicked,
                    user_gesture: action.is_user_gesture(),
                    uri: &uri,
                };
                let mut outcome = nav::decide(&request, &self.provider.borrow());
                if new_window && outcome == Outcome::Proceed && self.popups.get() >= MAX_POPUPS {
                    log::warn("a window was refused: too many pop-ups are open");
                    outcome = Outcome::Refuse;
                }
                match outcome {
                    Outcome::Proceed => false,
                    Outcome::Refuse => {
                        log::warn(&format!("refused a navigation to {}", describe(&uri)));
                        decision.ignore();
                        true
                    }
                    Outcome::OpenInBrowser => {
                        self.open_in_browser(&uri);
                        decision.ignore();
                        true
                    }
                }
            }
            PolicyDecisionType::Response => {
                // something the view cannot display (an archive, a document) is a download
                let unsupported = decision
                    .downcast_ref::<ResponsePolicyDecision>()
                    .is_some_and(|r| !r.is_mime_type_supported());
                if unsupported {
                    decision.download();
                }
                unsupported
            }
            _ => false,
        }
    }

    /// Hand an `http(s)` address to the default browser (at most twice a second).
    fn open_in_browser(&self, uri: &str) {
        let lower = uri.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            // `nav` never says so; a launcher must not depend on that alone
            log::error("refused to launch a non-web address");
            return;
        }
        let now = Instant::now();
        if self
            .last_browser_launch
            .get()
            .is_some_and(|t| now.duration_since(t) < BROWSER_COOLDOWN)
        {
            log::warn("a link was not opened: too many in a row");
            return;
        }
        self.last_browser_launch.set(Some(now));
        let uri = uri.to_owned();
        glib::spawn_future_local(async move {
            if let Err(e) =
                gio::AppInfo::launch_default_for_uri_future(&uri, None::<&gio::AppLaunchContext>)
                    .await
            {
                log::warn(&format!("could not open the browser: {}", e.message()));
            }
        });
    }

    /// A page asks for a window (`window.open`, or a link with `target=_blank` after the policy let
    /// it through). `window.open` reaches us here before any policy decision, so the decision is
    /// made here too: a sign-in popup gets a window of its own that shares the session; an address
    /// that belongs in the browser goes there; anything else is refused. `WebKit` insists on being
    /// handed a view either way, so a refused window gets one that is never shown and never loads.
    fn on_create(self: &Rc<Self>, opener: &WebView, action: &NavigationAction) -> gtk4::Widget {
        let mut action = action.clone();
        let uri = action
            .request()
            .and_then(|r| r.uri())
            .map(|u| u.to_string())
            .unwrap_or_default();
        let request = Navigation {
            new_window: true,
            link_clicked: false,
            user_gesture: action.is_user_gesture(),
            uri: &uri,
        };
        let popup = WebView::builder().related_view(opener).build();
        match nav::decide(&request, &self.provider.borrow()) {
            Outcome::Proceed if self.popups.get() < MAX_POPUPS => {
                if let Some(settings) = WebViewExt::settings(opener) {
                    popup.set_settings(&settings);
                }
                self.attach(&popup);
                self.show_popup(&popup);
            }
            Outcome::Proceed => {
                log::warn("a window was refused: too many pop-ups are open");
                refuse(&popup);
            }
            Outcome::OpenInBrowser => {
                self.open_in_browser(&uri);
                refuse(&popup);
            }
            Outcome::Refuse => {
                log::warn(&format!("refused a window for {}", describe(&uri)));
                refuse(&popup);
            }
        }
        popup.upcast()
    }

    /// Give `popup` a window of its own, shown when the page is ready.
    fn show_popup(self: &Rc<Self>, popup: &WebView) {
        let window = gtk4::Window::builder()
            .transient_for(&self.window)
            .default_width(POPUP_WIDTH)
            .default_height(POPUP_HEIGHT)
            .child(popup)
            .build();
        self.popups.set(self.popups.get() + 1);
        let me = self.clone();
        window.connect_destroy(move |_| me.popups.set(me.popups.get().saturating_sub(1)));
        let weak = window.downgrade();
        popup.connect_ready_to_show(move |_| {
            if let Some(w) = weak.upgrade() {
                w.present();
            }
        });
        let weak = window.downgrade();
        popup.connect_close(move |_| {
            if let Some(w) = weak.upgrade() {
                w.destroy();
            }
        });
        let weak = window.downgrade();
        popup.connect_title_notify(move |p| {
            if let (Some(w), Some(title)) = (weak.upgrade(), p.title()) {
                w.set_title(Some(&title));
            }
        });
    }

    /// Deny whatever the page asks for (notifications, location, camera, microphone, clipboard
    /// reading, device lists, pointer lock, storage access). The legacy app denied them all too —
    /// by not answering — and nothing the chat sites need depends on an answer.
    fn deny(&self, request: &PermissionRequest) -> bool {
        let kind = request.type_().name().to_owned();
        if self.denied.borrow_mut().insert(kind.clone()) {
            log::warn(&format!("denied a permission request ({kind})"));
        }
        request.deny();
        true
    }
}

/// Neutralise the view handed to `WebKit` for a window we do not want: it may not navigate anywhere,
/// and it is closed as soon as the main loop is idle.
fn refuse(popup: &WebView) {
    popup.connect_decide_policy(|_, decision, _| {
        decision.ignore();
        true
    });
    let doomed = popup.clone();
    glib::idle_add_local_once(move || doomed.try_close());
}

/// What to say about an address in the log: its scheme only. The rest may carry a sign-in code or
/// a token that has no business in a log.
fn describe(uri: &str) -> String {
    match uri.split_once(':') {
        Some((scheme, _))
            if !scheme.is_empty()
                && scheme.len() <= 20
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
        {
            format!("a {} address", scheme.to_ascii_lowercase())
        }
        _ => "an unreadable address".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_gets_the_scheme_and_nothing_else() {
        assert_eq!(describe("FILE:///etc/passwd"), "a file address");
        assert_eq!(describe("myapp://cb?code=SECRET"), "a myapp address");
        assert_eq!(
            describe("https://x.example/?token=SECRET"),
            "a https address"
        );
        assert_eq!(describe("no scheme at all"), "an unreadable address");
        assert_eq!(
            describe(&format!("{}:x", "s".repeat(30))),
            "an unreadable address"
        );
        assert_eq!(describe(""), "an unreadable address");
    }
}
