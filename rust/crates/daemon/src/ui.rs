//! The window and the web view: everything that touches GTK and `WebKit`.
//!
//! One plain `GtkWindow` — deliberately not a `GtkApplication`, which would turn the window class
//! (`app_id` on Wayland, `WM_CLASS` on X11) into the application id and break the compositor rules
//! users have for `llm-wrapper`. One `WebView`: only the provider on screen is kept alive.

use std::path::Path;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use webkit6::prelude::*;
use webkit6::{
    CacheModel, CookieAcceptPolicy, CookiePersistentStorage, MemoryPressureSettings,
    NetworkSession, Settings, WebContext, WebView, WebsiteDataTypes,
};

use crate::compat;
use crate::idle;

/// Name of the JavaScript world the app's own questions to the page run in.
const PROBE_WORLD: &str = "llm-wrapper-idle-probe";

const DEFAULT_WIDTH: i32 = 794;
const DEFAULT_HEIGHT: i32 = 1286;

/// Put the cursor in the chat box, like the legacy app did whenever it was summoned.
const FOCUS_PROMPT_JS: &str = "(function(){try{var el=document.querySelector('#prompt-textarea')||\
document.querySelector('textarea, [contenteditable=\"true\"]');if(el){el.focus();}}catch(e){}})();";

/// Window, web view and the `WebKit` objects behind it.
pub struct Ui {
    pub window: gtk4::Window,
    pub view: WebView,
    session: NetworkSession,
    context: WebContext,
    settings: Settings,
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

        let view = WebView::builder()
            .web_context(&context)
            .network_session(&session)
            .user_content_manager(&compat::content_manager())
            .settings(&settings)
            .build();
        let window = gtk4::Window::builder()
            .title("LLM")
            .default_width(DEFAULT_WIDTH)
            .default_height(DEFAULT_HEIGHT)
            .child(&view)
            .build();
        window.connect_close_request(|w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });
        Ok(Self {
            window,
            view,
            session,
            context,
            settings,
        })
    }

    // ---- asking the page -----------------------------------------------------------------------

    /// The address on screen.
    pub fn current_uri(&self) -> Option<String> {
        self.view.uri().map(|u| u.to_string())
    }

    /// Why the page is in use at this moment, if it is.
    pub fn busy(&self) -> Option<idle::Busy> {
        if !self.view.is_web_process_responsive() {
            Some(idle::Busy::Unresponsive)
        } else if self.view.is_loading() {
            Some(idle::Busy::Loading)
        } else if self.view.is_playing_audio() {
            Some(idle::Busy::Audio)
        } else {
            None
        }
    }

    /// Ask the page whether it holds something typed or attached that has not been sent.
    pub async fn has_draft(&self) -> idle::Draft {
        let asked = glib::future_with_timeout(
            Duration::from_secs(2),
            // in a JavaScript world of its own: the page cannot replace the functions the probe uses
            self.view
                .evaluate_javascript_future(idle::DRAFT_PROBE_JS, Some(PROBE_WORLD), None),
        )
        .await;
        let answer = match asked {
            Ok(Ok(value)) if value.is_boolean() => Some(value.to_boolean()),
            _ => None,
        };
        idle::draft_from(answer)
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
        self.view.load_uri(url);
    }

    pub fn reload(&self, bypass_cache: bool) {
        if bypass_cache {
            self.view.reload_bypass_cache();
        } else {
            self.view.reload();
        }
    }

    pub fn set_user_agent(&self, user_agent: &str) {
        self.settings.set_user_agent(Some(user_agent));
    }

    pub fn grab_focus(&self) {
        self.view.grab_focus();
        self.view
            .evaluate_javascript(FOCUS_PROMPT_JS, None, None, gio::Cancellable::NONE, |_| {});
    }

    /// Zoom by `delta`, never below 50 %; `None` resets to 100 %.
    pub fn zoom(&self, delta: Option<f64>) {
        let level = match delta {
            Some(d) => (self.view.zoom_level() + d).max(0.5),
            None => 1.0,
        };
        self.view.set_zoom_level(level);
    }

    /// F12: open or close the Web Inspector.
    pub fn toggle_inspector(&self) {
        if let Some(inspector) = self.view.inspector() {
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
