//! When a hidden window has been idle long enough, the page is let go: the web view is destroyed,
//! which ends the web process (the biggest part of the daemon's memory), and showing the window
//! again builds a new view and loads the page you were on.
//!
//! Letting go costs something — an unsent message, a half-filled form, a file attached and not yet
//! sent, a page in the middle of loading or playing sound would be lost or cut — so it only happens
//! when none of those is the case. If the page cannot be let go, the daemon falls back to the light
//! trim the legacy app did (drop the in-memory caches) and looks again later. This module is the
//! decision and the question put to the page; acting on it is `app.rs`.

use std::time::Duration;

/// Wait this long before looking again when the page is busy (loading, playing sound, a dialog or
/// a pop-up open, a download running).
pub const BUSY_RETRY: Duration = Duration::from_secs(60);

/// Why the page cannot be let go right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// The page is loading.
    Loading,
    /// The page is playing sound.
    Audio,
    /// A pop-up window is open.
    Popup,
    /// A file dialog is open.
    Dialog,
    /// A download is running.
    Download,
    /// The page does not answer: it is not a page to ask questions of.
    Unresponsive,
}

/// What the page says about unsent input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draft {
    /// Nothing typed, nothing attached.
    None,
    /// Something typed or attached.
    Some,
    /// The page did not answer, or answered something else.
    Unknown,
}

/// What to do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let the page go.
    Reap,
    /// Not now: look again after this long.
    LookAgain(Duration),
    /// Keep the page, drop the in-memory caches, and look again after the idle timeout.
    TrimAndKeep,
}

/// Decide, given what is known about the page. `busy` is checked first and is cheap; `draft` is
/// only worth asking for when nothing is busy.
pub fn decide(busy: Option<Busy>, draft: Draft) -> Decision {
    if busy.is_some() {
        return Decision::LookAgain(BUSY_RETRY);
    }
    match draft {
        Draft::None => Decision::Reap,
        Draft::Some | Draft::Unknown => Decision::TrimAndKeep,
    }
}

/// The question put to the page: is there something typed or attached and not sent? The script is
/// `draft_probe.js` (the end-to-end suite runs the same file in a page, so what is tested is what is
/// asked).
pub const DRAFT_PROBE_JS: &str = include_str!("draft_probe.js");

/// How the answer to [`DRAFT_PROBE_JS`] reads: `Some(true)` is "there is a draft".
pub fn draft_from(answer: Option<bool>) -> Draft {
    match answer {
        Some(false) => Draft::None,
        Some(true) => Draft::Some,
        None => Draft::Unknown,
    }
}

/// The address to bring back when the window is shown again: the page the user was on if it is a
/// page of this provider (or one the app lets into the window), otherwise the provider's own.
pub fn address_to_restore(current: Option<&str>, provider_url: &str) -> String {
    match current {
        Some(uri)
            if matches!(uri.split_once(':'), Some((s, _)) if s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"))
                && crate::nav::classify(uri, provider_url) == crate::nav::Verdict::Allow =>
        {
            uri.to_owned()
        }
        _ => provider_url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_page_is_looked_at_again_every_minute() {
        assert_eq!(BUSY_RETRY, Duration::from_secs(60));
    }

    #[test]
    fn a_quiet_page_without_a_draft_is_let_go() {
        assert_eq!(decide(None, Draft::None), Decision::Reap);
    }

    #[test]
    fn anything_busy_means_look_again_in_a_minute_and_the_draft_is_not_even_asked() {
        for busy in [
            Busy::Loading,
            Busy::Audio,
            Busy::Popup,
            Busy::Dialog,
            Busy::Download,
            Busy::Unresponsive,
        ] {
            for draft in [Draft::None, Draft::Some, Draft::Unknown] {
                assert_eq!(
                    decide(Some(busy), draft),
                    Decision::LookAgain(BUSY_RETRY),
                    "{busy:?}"
                );
            }
        }
    }

    #[test]
    fn a_draft_or_an_unreadable_answer_keeps_the_page() {
        assert_eq!(decide(None, Draft::Some), Decision::TrimAndKeep);
        assert_eq!(decide(None, Draft::Unknown), Decision::TrimAndKeep);
    }

    #[test]
    fn the_pages_answer_is_read_conservatively() {
        assert_eq!(draft_from(Some(false)), Draft::None);
        assert_eq!(draft_from(Some(true)), Draft::Some);
        assert_eq!(draft_from(None), Draft::Unknown);
    }

    #[test]
    fn the_page_you_were_on_comes_back_if_the_app_would_show_it_anyway() {
        let provider = "https://chatgpt.com";
        let r = |cur: Option<&str>| address_to_restore(cur, provider);
        assert_eq!(
            r(Some("https://chatgpt.com/c/abc-123")),
            "https://chatgpt.com/c/abc-123"
        );
        assert_eq!(
            r(Some("https://auth.openai.com/authorize?x=1")),
            "https://auth.openai.com/authorize?x=1"
        );
        // what is not a page of the provider, or not a web page at all, is not brought back
        assert_eq!(r(Some("https://example.org/")), provider);
        assert_eq!(r(Some("http://chatgpt.com/c/1")), provider);
        assert_eq!(r(Some("about:blank")), provider);
        assert_eq!(r(Some("blob:https://chatgpt.com/1-2")), provider);
        assert_eq!(r(Some("data:text/html,<p>")), provider);
        assert_eq!(r(Some("file:///etc/passwd")), provider);
        assert_eq!(r(Some("")), provider);
        assert_eq!(r(None), provider);
    }

    #[test]
    fn the_probe_is_one_expression_that_answers_a_boolean_and_keeps_the_page_on_error() {
        let js = DRAFT_PROBE_JS.trim();
        assert!(
            js.contains("\n(function () {"),
            "an immediately invoked function"
        );
        assert!(js.ends_with("})()"));
        assert!(js.contains("} catch (x) {\n    return true;"));
        assert_eq!(js.matches('{').count(), js.matches('}').count());
        assert_eq!(js.matches('(').count(), js.matches(')').count());
        for kind in [
            "textarea",
            "contenteditable",
            "password",
            "'file'",
            "e.files.length",
            "innerText",
        ] {
            assert!(js.contains(kind), "{kind}");
        }
    }
}
