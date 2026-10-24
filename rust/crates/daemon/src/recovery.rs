//! Getting the page back when something outside our control breaks it.
//!
//! * **Network failure.** The page that failed is remembered (the legacy app went back to the
//!   provider's root instead, losing the conversation you were in) and retried when the network
//!   comes back and every 30 s meanwhile, until a retry succeeds or you go elsewhere.
//! * **Web process crash.** The page is reloaded, at most three times a minute so a page that
//!   crashes on load cannot spin forever (the legacy app left a blank view until you intervened).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{gio, glib};
use webkit6::prelude::*;
use webkit6::{LoadEvent, WebProcessTerminationReason, WebView};

use crate::log;

const RETRY_SECONDS: u32 = 30;
const MAX_CRASH_RELOADS: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Inner {
    /// The page whose load failed and is waiting to be retried.
    failed: Option<String>,
    /// A retry is in flight.
    retrying: bool,
    /// ...and it has failed again.
    failed_again: bool,
    timer: Option<glib::SourceId>,
    /// Last page we know loaded or were asked to load; what to reload after a crash.
    last_uri: Option<String>,
    crashes: VecDeque<Instant>,
}

/// Failure and crash handling for the web view of the moment. The view can be let go and built
/// again (see `reap.rs`); this outlives it and is attached to each new one.
pub struct Recovery {
    view: RefCell<glib::WeakRef<WebView>>,
    inner: RefCell<Inner>,
}

/// Same page, ignoring a trailing slash and the fragment.
fn same_page(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split('#')
            .next()
            .unwrap_or(s)
            .trim_end_matches('/')
            .to_owned()
    };
    norm(a) == norm(b)
}

impl Recovery {
    /// Watch the network, once for the life of the daemon.
    pub fn new() -> Rc<Self> {
        let this = Rc::new(Self {
            view: RefCell::new(glib::WeakRef::new()),
            inner: RefCell::default(),
        });
        let r = this.clone();
        gio::NetworkMonitor::default().connect_network_changed(move |_, available| {
            if available {
                r.attempt();
            }
        });
        this
    }

    /// Watch `view` from now on (it replaces the one watched before).
    pub fn attach(self: &Rc<Self>, view: &WebView) {
        self.view.borrow().set(Some(view));
        let this = self;

        let r = this.clone();
        view.connect_load_failed(move |_, _event, uri, error| {
            let cancelled = error.matches(webkit6::NetworkError::Cancelled)
                || error.matches(webkit6::PolicyError::FrameLoadInterruptedByPolicyChange);
            if !cancelled {
                r.on_failed(uri);
            }
            false // let WebKit show its error page
        });
        let r = this.clone();
        view.connect_load_changed(move |_, event| {
            if event == LoadEvent::Finished {
                r.on_finished();
            }
        });
        let r = this.clone();
        view.connect_uri_notify(move |view| {
            if let Some(uri) = view.uri() {
                r.on_uri(uri.as_str());
            }
        });
        let r = this.clone();
        view.connect_web_process_terminated(move |_, reason| r.on_crash(reason));
    }

    /// Forget every failure and the page to come back to: the view is being let go on purpose.
    pub fn forget(&self) {
        let mut s = self.inner.borrow_mut();
        if let Some(t) = s.timer.take() {
            t.remove();
        }
        *s = Inner::default();
    }

    /// The program itself is loading `url`: that supersedes any earlier failure.
    pub fn note_requested(&self, url: &str) {
        let mut s = self.inner.borrow_mut();
        s.failed = None;
        s.retrying = false;
        if let Some(t) = s.timer.take() {
            t.remove();
        }
        s.last_uri = Some(url.to_owned());
    }

    fn on_failed(self: &Rc<Self>, uri: &str) {
        let mut s = self.inner.borrow_mut();
        s.failed = Some(uri.to_owned());
        s.failed_again = true;
        if s.timer.is_none() {
            let me = self.clone();
            s.timer = Some(glib::timeout_add_seconds_local_once(
                RETRY_SECONDS,
                move || {
                    me.inner.borrow_mut().timer = None; // fired: nothing left to remove
                    me.attempt();
                },
            ));
        }
    }

    fn on_finished(&self) {
        let mut s = self.inner.borrow_mut();
        if s.retrying {
            s.retrying = false;
            if !s.failed_again {
                log::info("the page is back");
                s.failed = None;
                if let Some(t) = s.timer.take() {
                    t.remove();
                }
            }
        }
    }

    fn on_uri(&self, uri: &str) {
        let mut s = self.inner.borrow_mut();
        if uri.is_empty() {
            return;
        }
        s.last_uri = Some(uri.to_owned());
        // the user went somewhere else: stop trying to bring the old page back
        let moved_on = s.failed.as_deref().is_some_and(|f| !same_page(f, uri));
        if moved_on && !s.retrying {
            s.failed = None;
            if let Some(t) = s.timer.take() {
                t.remove();
            }
        }
    }

    /// Retry the page that failed, now.
    fn attempt(&self) {
        let (Some(view), uri) = (
            self.view.borrow().upgrade(),
            self.inner.borrow().failed.clone(),
        ) else {
            return;
        };
        let Some(uri) = uri else { return };
        {
            let mut s = self.inner.borrow_mut();
            if s.retrying {
                return;
            }
            s.retrying = true;
            s.failed_again = false;
        }
        view.load_uri(&uri);
    }

    fn on_crash(self: &Rc<Self>, reason: WebProcessTerminationReason) {
        if !matches!(
            reason,
            WebProcessTerminationReason::Crashed | WebProcessTerminationReason::ExceededMemoryLimit
        ) {
            return; // we asked for it
        }
        let uri = {
            let mut s = self.inner.borrow_mut();
            let now = Instant::now();
            while s
                .crashes
                .front()
                .is_some_and(|t| now.duration_since(*t) > CRASH_WINDOW)
            {
                s.crashes.pop_front();
            }
            if s.crashes.len() >= MAX_CRASH_RELOADS {
                log::error("the web process keeps crashing; not reloading again");
                return;
            }
            s.crashes.push_back(now);
            s.last_uri.clone()
        };
        log::warn(&format!(
            "the web process ended unexpectedly ({reason:?}); reloading"
        ));
        let me = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            if let (Some(view), Some(uri)) = (me.view.borrow().upgrade(), uri) {
                view.load_uri(&uri);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::same_page;

    #[test]
    fn pages_compare_without_slash_or_fragment() {
        assert!(same_page("https://x.example/a", "https://x.example/a/"));
        assert!(same_page("https://x.example/a#top", "https://x.example/a"));
        assert!(same_page("http://127.0.0.1:1", "http://127.0.0.1:1/"));
        assert!(!same_page("https://x.example/a", "https://x.example/b"));
        assert!(!same_page("https://x.example/a?x=1", "https://x.example/a"));
    }
}
