//! User-Agent overrides, idle timeouts and terminal-safe text.

use std::fmt::Write as _;

use crate::error::{IdleProblem, UserAgentProblem, ValidationError};
use crate::limits::{MAX_IDLE_TIMEOUT_MINUTES, MAX_USER_AGENT_LEN};

/// A validated User-Agent override: 1-512 bytes of printable ASCII, so it can never inject a header
/// line break or a control sequence.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UserAgent(String);

impl UserAgent {
    /// Validate `s` and wrap it.
    pub fn parse(s: &str) -> Result<Self, ValidationError> {
        let problem = if s.is_empty() {
            Some(UserAgentProblem::Empty)
        } else if s.len() > MAX_USER_AGENT_LEN {
            Some(UserAgentProblem::TooLong)
        } else if !s.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            Some(UserAgentProblem::BadChar)
        } else {
            None
        };
        match problem {
            Some(p) => Err(ValidationError::UserAgent(p)),
            None => Ok(Self(s.to_owned())),
        }
    }

    /// The validated text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Parse `--idle-timeout`: an optionally signed run of at most 18 ASCII digits.
///
/// Like the legacy `max(1, value)` anything below one minute becomes one minute; anything above one
/// year is refused (the legacy code would overflow the `GLib` timer instead).
pub fn parse_idle_timeout(s: &str) -> Result<u32, ValidationError> {
    let err = |p| Err(ValidationError::IdleTimeout(p));
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return err(IdleProblem::NotANumber);
    }
    let Ok(magnitude) = digits.parse::<u64>() else {
        return err(IdleProblem::NotANumber);
    };
    if negative || magnitude == 0 {
        return Ok(1);
    }
    match u32::try_from(magnitude) {
        Ok(m) if m <= MAX_IDLE_TIMEOUT_MINUTES => Ok(m),
        _ => err(IdleProblem::TooLarge),
    }
}

/// Make text received from another process safe to print on a terminal: control characters, the
/// bidirectional override characters and anything else that could rewrite what the user reads
/// become `?`.
pub fn sanitize_for_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let hostile = c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if hostile {
            out.push('?');
        } else {
            let _ = out.write_char(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_rules() {
        assert!(UserAgent::parse("Mozilla/5.0 (X11; Linux x86_64) Test/1.0").is_ok());
        assert!(UserAgent::parse(&"a".repeat(512)).is_ok());
        for bad in [
            "",
            "tab\there",
            "a\r\nX: 1",
            "na\u{ef}ve",
            "\u{7f}",
            "nul\0",
        ] {
            assert!(UserAgent::parse(bad).is_err(), "{bad:?}");
        }
        assert!(UserAgent::parse(&"a".repeat(513)).is_err());
    }

    #[test]
    fn idle_timeout_matches_the_legacy_clamp_and_adds_bounds() {
        for (input, want) in [
            ("5", 5),
            ("+7", 7),
            ("0", 1),
            ("-5", 1),
            ("-0", 1),
            ("525600", 525_600),
            ("000012", 12),
        ] {
            assert_eq!(parse_idle_timeout(input), Ok(want), "{input}");
        }
        for bad in [
            "", "+", "-", "abc", "5.5", " 5", "5 ", "1_0", "\u{0665}", "0x10", "5m",
        ] {
            assert_eq!(
                parse_idle_timeout(bad),
                Err(ValidationError::IdleTimeout(IdleProblem::NotANumber)),
                "{bad:?}"
            );
        }
        for big in ["525601", "99999999999", "999999999999999999"] {
            assert_eq!(
                parse_idle_timeout(big),
                Err(ValidationError::IdleTimeout(IdleProblem::TooLarge)),
                "{big}"
            );
        }
        assert!(parse_idle_timeout("1000000000000000000").is_err());
    }

    #[test]
    fn terminal_sanitising() {
        assert_eq!(
            sanitize_for_terminal("plain text, \u{e9}"),
            "plain text, \u{e9}"
        );
        assert_eq!(sanitize_for_terminal("a\u{1b}[2Jb\nc\u{7}"), "a?[2Jb?c?");
        assert_eq!(sanitize_for_terminal("ok\u{202e}evil"), "ok?evil");
    }
}
