//! The window and the web view: everything that touches GTK and `WebKit`.
//!
//! One plain `GtkWindow` — deliberately not a `GtkApplication`, which would turn the window class
//! (`app_id` on Wayland, `WM_CLASS` on X11) into the application id and break the compositor rules
//! users have for `llm-wrapper`. One `WebView`: only the provider on screen is kept alive, and it
//! can be let go while the window is hidden and built again when the window is shown (`reap.rs`).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use webkit6::prelude::*;
use webkit6::{
    CacheModel, CookieAcceptPolicy, CookiePersistentStorage, LoadEvent, MemoryPressureSettings,
    NetworkSession, Settings, UserContentManager, WebContext, WebView, WebsiteDataTypes,
};

use crate::compat;
use crate::reap;

/// Name of the JavaScript world the app's own questions to the page run in.
const PROBE_WORLD: &str = "llm-wrapper-idle-probe";

const DEFAULT_WIDTH: i32 = 794;
const DEFAULT_HEIGHT: i32 = 1286;

/// Put the cursor in the chat box, like the legacy app did whenever it was summoned.
const FOCUS_PROMPT_JS: &str = "(function(){try{var el=document.querySelector('#prompt-textarea')||\
document.querySelector('textarea, [contenteditable=\"true\"]');if(el){el.focus();}}catch(e){}})();";

/// What is done with every view that is built.
type ViewHook = Rc<dyn Fn(&WebView)>;

/// Window, web view and the `WebKit` objects behind it.
pub struct Ui {
    pub window: gtk4::Window,
    /// `None` while the page is let go.
    view: RefCell<Option<WebView>>,
    session: NetworkSession,
    context: WebContext,
    settings: Settings,
    content: UserContentManager,
    /// What the user set with the zoom keys; a new view starts there.
    zoom: Cell<f64>,
    /// Called with every view built, to attach the policy and the recovery to it.
    on_new_view: RefCell<Option<ViewHook>>,
    /// Put the cursor in the chat box when the page that is loading now has loaded.
    focus_when_loaded: Rc<Cell<bool>>,
}

/// The part of a URL a person recognises, for the window title.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#', ':']).next().unwrap_or(rest)
}

impl Ui {
    /// Build the window (hidden) and the web view. `data` and `cache` are the verified private
    /// directories for this instance.
    pub fn new(data: &Path, cache: &Path) -> Result<Self, String> {
        let text = |p: &Path| {
            p.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
        };
        let (data_dir, cache_dir) = (text(data)?, text(cache)?);

        // Bounding memory (strict threshold first: WebKit requires conservative < strict, and its
        // defaults are 0.33 and 0.5): the network process through a process-wide setting, the web processes
        // through the context they are created from. (A limit set on the former does nothing to the
        // latter: measured, 779 MB with and without it.)
        let mut network_limits = MemoryPressureSettings::new();
        network_limits.set_memory_limit(512);
        network_limits.set_strict_threshold(0.75);
        network_limits.set_conservative_threshold(0.5);
        network_limits.set_poll_interval(5.0);
        NetworkSession::set_memory_pressure_settings(&mut network_limits);
        let mut web_limits = MemoryPressureSettings::new();
        web_limits.set_memory_limit(1024);
        web_limits.set_strict_threshold(0.75);
        web_limits.set_conservative_threshold(0.5);
        web_limits.set_poll_interval(5.0);

        let session = NetworkSession::new(Some(&data_dir), Some(&cache_dir));
        // Cross-site tracking prevention would break single-sign-on logins, as it did for the legacy app.
        session.set_itp_enabled(false);
        session.set_persistent_credential_storage_enabled(true);
        if let Some(cookies) = session.cookie_manager() {
            cookies.set_accept_policy(CookieAcceptPolicy::Always);
            cookies.set_persistent_storage(
                &format!("{data_dir}/cookies.sqlite"),
                CookiePersistentStorage::Sqlite,
            );
        }

        let context = WebContext::builder()
            .memory_pressure_settings(&web_limits)
            .build();
        context.set_cache_model(CacheModel::WebBrowser);

        let settings = Settings::new();
        settings.set_enable_smooth_scrolling(true);
        settings.set_javascript_can_access_clipboard(true); // image paste with Ctrl+V
        settings.set_enable_page_cache(true);
        settings.set_enable_developer_extras(true); // F12
        settings.set_enable_site_specific_quirks(true);
        settings.set_javascript_can_open_windows_automatically(true); // sign-in popups open after async work
        settings.set_enable_media_stream(false); // no voice mode, so no microphone prompts
        settings.set_enable_encrypted_media(false);
        settings.set_user_agent(Some(&compat::user_agent()));

        let window = gtk4::Window::builder()
            .title("LLM")
            .default_width(DEFAULT_WIDTH)
            .default_height(DEFAULT_HEIGHT)
            .build();
        window.connect_close_request(|w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });
        Ok(Self {
            window,
            view: RefCell::new(None),
            session,
            context,
            settings,
            content: compat::content_manager(),
            zoom: Cell::new(1.0),
            on_new_view: RefCell::new(None),
            focus_when_loaded: Rc::default(),
        })
    }

    // ---- the web view's life -----------------------------------------------------------------

    /// What to do with every view that is built (attach the policy, the recovery...).
    pub fn on_new_view(&self, hook: impl Fn(&WebView) + 'static) {
        *self.on_new_view.borrow_mut() = Some(Rc::new(hook));
    }

    /// The view, if the page has not been let go.
    pub fn view(&self) -> Option<WebView> {
        self.view.borrow().clone()
    }

    /// Is there a page, or has it been let go?
    pub fn has_view(&self) -> bool {
        self.view.borrow().is_some()
    }

    /// Build the view if there is none. Returns it when it was built just now.
    pub fn ensure_view(&self) -> Option<WebView> {
        if self.view.borrow().is_some() {
            return None;
        }
        let view = WebView::builder()
            .web_context(&self.context)
            .network_session(&self.session)
            .user_content_manager(&self.content)
            .settings(&self.settings)
            .build();
        view.set_zoom_level(self.zoom.get());
        let focus = self.focus_when_loaded.clone();
        view.connect_load_changed(move |view, event| {
            if event == LoadEvent::Finished && focus.replace(false) {
                view.grab_focus();
                view.evaluate_javascript(
                    FOCUS_PROMPT_JS,
                    None,
                    None,
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });
        let hook = self.on_new_view.borrow().clone();
        if let Some(hook) = hook {
            hook(&view);
        }
        self.window.set_child(Some(&view));
        *self.view.borrow_mut() = Some(view.clone());
        Some(view)
    }

    /// Let the page go: take the view out of the window and drop it, which ends its web process.
    /// Returns the address it was showing.
    pub fn drop_view(&self) -> Option<String> {
        let view = self.view.borrow_mut().take()?;
        let uri = view.uri().map(|u| u.to_string());
        // The window keeps the widget that has the focus alive (measured: a view that was shown and
        // then dropped lives on, with its web process, until the focus is cleared).
        gtk4::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk4::Widget>);
        self.window.set_child(None::<&gtk4::Widget>);
        // Destroying the view is not enough to end its web process: WebKit may keep it for the next
        // page (measured: still running 40 s later), so it is told to go.
        view.terminate_web_process();
        // The memory comes back only if nothing else holds the view; say so if something does.
        let weak = view.downgrade();
        drop(view);
        glib::timeout_add_seconds_local_once(3, move || {
            if weak.upgrade().is_some() {
                crate::log::warn("the web view outlived being let go: its memory was not returned");
            }
        });
        uri
    }

    /// The page being loaded is the one the user is waiting for: focus the chat box when it is there.
    pub fn focus_when_loaded(&self) {
        self.focus_when_loaded.set(true);
    }

    /// The address on screen.
    pub fn current_uri(&self) -> Option<String> {
        self.view().and_then(|v| v.uri()).map(|u| u.to_string())
    }

    /// Why the page cannot be let go at this moment, if it cannot.
    pub fn busy(&self) -> Option<reap::Busy> {
        let view = self.view()?;
        if !view.is_web_process_responsive() {
            Some(reap::Busy::Unresponsive)
        } else if view.is_loading() {
            Some(reap::Busy::Loading)
        } else if view.is_playing_audio() {
            Some(reap::Busy::Audio)
        } else {
            None
        }
    }

    /// Ask the page whether it holds something typed or attached that has not been sent.
    pub async fn has_draft(&self) -> reap::Draft {
        let Some(view) = self.view() else {
            return reap::Draft::None;
        };
        let asked = glib::future_with_timeout(
            Duration::from_secs(2),
            // in a JavaScript world of its own: the page cannot replace the functions the probe uses
            view.evaluate_javascript_future(reap::DRAFT_PROBE_JS, Some(PROBE_WORLD), None),
        )
        .await;
        let answer = match asked {
            Ok(Ok(value)) if value.is_boolean() => Some(value.to_boolean()),
            _ => None,
        };
        reap::draft_from(answer)
    }

    // ---- window --------------------------------------------------------------------------------

    /// The network session (downloads are observed on it).
    pub fn session(&self) -> &NetworkSession {
        &self.session
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn show(&self) {
        self.window.present();
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    pub fn toggle(&self) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// Provider name, or the host for an ad-hoc URL.
    pub fn set_title_for(&self, name: &str) {
        let title = if name.contains("://") {
            host_of(name)
        } else {
            name
        };
        self.window.set_title(Some(title));
    }

    // ---- page ----------------------------------------------------------------------------------

    pub fn load(&self, url: &str) {
        if let Some(view) = self.view() {
            view.load_uri(url);
        }
    }

    pub fn reload(&self, bypass_cache: bool) {
        if let Some(view) = self.view() {
            if bypass_cache {
                view.reload_bypass_cache();
            } else {
                view.reload();
            }
        }
    }

    pub fn set_user_agent(&self, user_agent: &str) {
        self.settings.set_user_agent(Some(user_agent));
    }

    pub fn grab_focus(&self) {
        if let Some(view) = self.view() {
            view.grab_focus();
            view.evaluate_javascript(FOCUS_PROMPT_JS, None, None, gio::Cancellable::NONE, |_| {});
        }
    }

    /// Zoom by `delta`, never below 50 %; `None` resets to 100 %. Kept for the views to come.
    pub fn zoom(&self, delta: Option<f64>) {
        let level = match delta {
            Some(d) => (self.zoom.get() + d).max(0.5),
            None => 1.0,
        };
        self.zoom.set(level);
        if let Some(view) = self.view() {
            view.set_zoom_level(level);
        }
    }

    /// F12: open or close the Web Inspector.
    pub fn toggle_inspector(&self) {
        let inspector = self.view().and_then(|v| v.inspector());
        if let Some(inspector) = inspector {
            if inspector.is_attached() {
                inspector.close();
            } else {
                inspector.show();
            }
        }
    }

    // ---- memory --------------------------------------------------------------------------------

    fn clear(&self, types: WebsiteDataTypes) {
        if let Some(manager) = self.session.website_data_manager() {
            manager.clear(
                types,
                glib::TimeSpan::from_seconds(0),
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    /// Release in-memory caches; disk cache, cookies and service workers stay.
    pub fn soft_clean(&self) {
        self.clear(WebsiteDataTypes::MEMORY_CACHE);
    }

    /// Purge disk and DOM caches; login cookies stay.
    pub fn hard_clean(&self) {
        self.clear(
            WebsiteDataTypes::DISK_CACHE
                | WebsiteDataTypes::MEMORY_CACHE
                | WebsiteDataTypes::DOM_CACHE
                | WebsiteDataTypes::OFFLINE_APPLICATION_CACHE,
        );
    }

    /// Hidden for a long time: stop keeping pages in memory. Showing the window restores it.
    pub fn set_trimmed(&self, trimmed: bool) {
        self.context.set_cache_model(if trimmed {
            CacheModel::DocumentViewer
        } else {
            CacheModel::WebBrowser
        });
    }
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn titles_show_the_host() {
        assert_eq!(
            host_of("https://chat.mistral.ai/c/1?x=2"),
            "chat.mistral.ai"
        );
        assert_eq!(host_of("http://localhost:3000/"), "localhost");
        assert_eq!(host_of("https://example.com"), "example.com");
        assert_eq!(host_of("no-scheme.example/path"), "no-scheme.example");
    }
}
