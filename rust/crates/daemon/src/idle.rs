//! What happens to a page while its window stays hidden and idle.
//!
//! The provider's home page is cheap and is kept loaded, so showing the window is instant. A page
//! the user went on to (a conversation, a long page) is not: after the idle timeout the hidden view
//! is sent back to the provider's home in a new web process, which lets go of everything the page
//! held (a long chat can hold a lot; loading home into the same process gave back almost nothing).
//! The page you come back to is the home page, already loaded, not the conversation: that one is a
//! click away and is loaded from the site when asked for.
//!
//! Going home costs something — an unsent message, a half-filled form, a file attached and not yet
//! sent, a page in the middle of loading or playing sound would be lost or cut — so it only happens
//! when none of those is the case. If the page cannot be sent home, the daemon falls back to the
//! light trim the legacy app did (drop the in-memory caches) and looks again later. This module is
//! the decision and the question put to the page; acting on it is `app.rs`.

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
    /// Send the page back to the provider's home.
    GoHome,
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
        Draft::None => Decision::GoHome,
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

/// Is `current` the provider's home page already (nothing to go back from)? Same address, ignoring
/// a trailing slash, the query and the fragment, and the case of the host.
pub fn is_home(current: Option<&str>, provider_url: &str) -> bool {
    fn key(u: &str) -> String {
        let no_frag = u.split('#').next().unwrap_or(u);
        let no_query = no_frag.split('?').next().unwrap_or(no_frag);
        no_query.trim_end_matches('/').to_ascii_lowercase()
    }
    current.is_some_and(|c| !c.is_empty() && key(c) == key(provider_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_page_is_looked_at_again_every_minute() {
        assert_eq!(BUSY_RETRY, Duration::from_secs(60));
    }

    #[test]
    fn a_quiet_page_without_a_draft_goes_home() {
        assert_eq!(decide(None, Draft::None), Decision::GoHome);
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
    fn the_home_page_is_recognised_whatever_its_decoration() {
        let home = "https://chatgpt.com";
        assert!(is_home(Some("https://chatgpt.com/"), home));
        assert!(is_home(Some("https://chatgpt.com/?model=x#top"), home));
        assert!(is_home(Some("HTTPS://ChatGPT.com"), home));
        assert!(!is_home(Some("https://chatgpt.com/c/abc-123"), home));
        assert!(!is_home(Some("https://auth.openai.com/"), home));
        assert!(!is_home(Some(""), home));
        assert!(!is_home(None, home));
        // a provider with a path of its own
        assert!(is_home(
            Some("http://127.0.0.1:1/app/"),
            "http://127.0.0.1:1/app"
        ));
        assert!(!is_home(
            Some("http://127.0.0.1:1/other"),
            "http://127.0.0.1:1/app"
        ));
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
