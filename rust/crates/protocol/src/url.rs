//! Provider URLs.
//!
//! This is a *validator*, not a general URL parser: it accepts one narrow, canonical shape and
//! refuses everything it does not fully understand. The goal is that the string we accept means
//! the same thing to us, to `GLib` and to `WebKit` (no parser differentials), and that it cannot smuggle
//! credentials, odd encodings or cleartext transport to a remote host into the provider list.
//!
//! Accepted: `https://host[:port][/path?query#fragment]`, and the same with `http` when the host is
//! `localhost` or a loopback IP literal.

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::error::{UrlProblem, ValidationError};
use crate::limits::MAX_URL_LEN;

/// A validated provider URL. The text is kept exactly as written (no normalisation).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderUrl(String);

#[derive(Debug, PartialEq, Eq)]
enum Host<'a> {
    Name(&'a str),
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

impl Host<'_> {
    fn is_loopback(&self) -> bool {
        match self {
            Host::Name(n) => n.eq_ignore_ascii_case("localhost"),
            Host::V4(a) => a.is_loopback(),
            Host::V6(a) => a.is_loopback(),
        }
    }
}

impl ProviderUrl {
    /// Validate `input` and wrap it.
    pub fn parse(input: &str) -> Result<Self, ValidationError> {
        validate(input).map_err(ValidationError::Url)?;
        Ok(Self(input.to_owned()))
    }

    /// The validated text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Printable ASCII except the characters that are never legal in a URL (and are classic sources of
/// parser differentials: backslash, quotes, angle brackets, caret, backtick, braces, pipe).
fn is_url_byte(b: u8) -> bool {
    (0x21..=0x7e).contains(&b)
        && !matches!(
            b,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        )
}

fn validate(s: &str) -> Result<(), UrlProblem> {
    if s.is_empty() {
        return Err(UrlProblem::Empty);
    }
    if s.len() > MAX_URL_LEN {
        return Err(UrlProblem::TooLong);
    }
    if !s.bytes().all(is_url_byte) {
        return Err(UrlProblem::BadChar);
    }
    let (scheme, rest) = s.split_once("://").ok_or(UrlProblem::MissingScheme)?;
    let https = if scheme.eq_ignore_ascii_case("https") {
        true
    } else if scheme.eq_ignore_ascii_case("http") {
        false
    } else {
        return Err(UrlProblem::UnsupportedScheme);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains('@') {
        return Err(UrlProblem::Userinfo);
    }
    let host = parse_authority(authority)?;
    check_percent_escapes(tail)?;
    if !https && !host.is_loopback() {
        return Err(UrlProblem::InsecureHttp);
    }
    Ok(())
}

fn parse_authority(authority: &str) -> Result<Host<'_>, UrlProblem> {
    if authority.is_empty() {
        return Err(UrlProblem::EmptyHost);
    }
    if let Some(after_bracket) = authority.strip_prefix('[') {
        let (inner, after) = after_bracket.split_once(']').ok_or(UrlProblem::BadIpv6)?;
        let ip: Ipv6Addr = inner.parse().map_err(|_| UrlProblem::BadIpv6)?;
        if !after.is_empty() {
            check_port(after.strip_prefix(':').ok_or(UrlProblem::BadPort)?)?;
        }
        return Ok(Host::V6(ip));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if let Some(p) = port {
        check_port(p)?;
    }
    if host.is_empty() {
        return Err(UrlProblem::EmptyHost);
    }
    parse_host(host)
}

fn check_port(p: &str) -> Result<(), UrlProblem> {
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UrlProblem::BadPort);
    }
    match p.parse::<u32>() {
        Ok(1..=65535) => Ok(()),
        _ => Err(UrlProblem::BadPort),
    }
}

fn parse_host(host: &str) -> Result<Host<'_>, UrlProblem> {
    if host.len() > 253 {
        return Err(UrlProblem::BadHost);
    }
    let mut last = "";
    for label in host.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return Err(UrlProblem::BadHost);
        }
        last = label;
    }
    // WHATWG: a host whose last label is a number is an IPv4 address, whatever it looks like
    // (`2130706433`, `0x7f.1`, `127.1`). Accept only the canonical dotted quad, so that nothing we
    // call a "DNS name" is reinterpreted as an address by another parser.
    if ends_in_number(last) {
        return host
            .parse::<Ipv4Addr>()
            .map(Host::V4)
            .map_err(|_| UrlProblem::BadHost);
    }
    Ok(Host::Name(host))
}

fn ends_in_number(label: &str) -> bool {
    let all_digits = !label.is_empty() && label.bytes().all(|b| b.is_ascii_digit());
    let hex = label
        .strip_prefix("0x")
        .or_else(|| label.strip_prefix("0X"))
        .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
    all_digits || hex
}

fn check_percent_escapes(tail: &str) -> Result<(), UrlProblem> {
    let b = tail.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let ok = b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit);
            if !ok {
                return Err(UrlProblem::BadPercent);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<(), UrlProblem> {
        validate(s)
    }

    #[test]
    fn accepts_canonical_urls() {
        for ok in [
            "https://chatgpt.com",
            "https://chatgpt.com/",
            "HTTPS://ChatGPT.com/Path?q=a%20b#frag",
            "https://chat.mistral.ai",
            "https://gemini.google.com/app",
            "https://x.example:8443/a/b?c=d&e=f",
            "https://1.2.3.4/",
            "https://[2001:db8::1]:443/",
            "https://a-b.c-d.example/",
            "https://example.com?x=1",
            "http://localhost",
            "http://LOCALHOST:3000/",
            "http://127.0.0.1:8080/",
            "http://127.1.2.3/",
            "http://[::1]:8080/",
        ] {
            assert_eq!(p(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn refuses_what_it_does_not_understand() {
        use UrlProblem::*;
        let cases: &[(&str, UrlProblem)] = &[
            ("", Empty),
            ("example.com", MissingScheme),
            ("https:/example.com", MissingScheme),
            ("ftp://example.com", UnsupportedScheme),
            ("javascript:alert(1)", MissingScheme),
            ("file:///etc/passwd", UnsupportedScheme),
            ("data://text/html,x", UnsupportedScheme),
            ("https:// example.com", BadChar),
            ("https://exa mple.com", BadChar),
            ("https://example.com/\n", BadChar),
            ("https://example.com/\\evil", BadChar),
            ("https://example.com/<x>", BadChar),
            ("https://\u{e9}xample.com", BadChar),
            ("https://user@example.com", Userinfo),
            ("https://user:pw@example.com", Userinfo),
            ("https://example.com@evil.com", Userinfo),
            ("https://example.com:80@evil.com/", Userinfo),
            ("https://", EmptyHost),
            ("https:///path", EmptyHost),
            ("https://:443", EmptyHost),
            ("https://example.com:", BadPort),
            ("https://example.com:0", BadPort),
            ("https://example.com:65536", BadPort),
            ("https://example.com:80a", BadPort),
            ("https://example.com:123456", BadPort),
            ("https://.example.com", BadHost),
            ("https://example..com", BadHost),
            ("https://example.com.", BadHost),
            ("https://-a.example", BadHost),
            ("https://a-.example", BadHost),
            ("https://under_score.example", BadHost),
            ("https://exa%6dple.com", BadHost),
            ("https://[::1", BadIpv6),
            ("https://[::g]", BadIpv6),
            ("https://[::1%25eth0]/", BadIpv6),
            ("https://[::1]x", BadPort),
            ("https://example.com/%", BadPercent),
            ("https://example.com/%4", BadPercent),
            ("https://example.com/%zz", BadPercent),
            ("http://example.com", InsecureHttp),
            ("http://192.168.1.10:3000", InsecureHttp),
            ("http://localhost.evil.com", InsecureHttp),
            ("http://[::ffff:127.0.0.1]/", InsecureHttp),
        ];
        for (input, want) in cases {
            assert_eq!(p(input), Err(*want), "{input:?}");
        }
        assert_eq!(
            p(&format!("https://{}.example", "a".repeat(64))),
            Err(BadHost)
        );
        assert_eq!(
            p(&format!("https://example.com/{}", "a".repeat(MAX_URL_LEN))),
            Err(TooLong)
        );
    }

    #[test]
    fn numeric_hosts_must_be_canonical_dotted_quads() {
        // Each of these is `127.0.0.1` to a WHATWG parser; none may pass as a "DNS name".
        for sneaky in [
            "https://2130706433/",
            "https://0x7f000001/",
            "https://0x7f.1/",
            "https://127.1/",
            "https://017700000001/",
            "https://127.0.0.01/",
            "https://1.2.3.4.5/",
            "https://256.1.1.1/",
            "https://example.0x1/",
            "https://example.123/",
        ] {
            assert_eq!(p(sneaky), Err(UrlProblem::BadHost), "{sneaky}");
        }
        assert_eq!(p("https://1.2.3.4/"), Ok(()));
        assert_eq!(p("https://example.com1/"), Ok(()));
        assert_eq!(p("https://example.x1/"), Ok(()));
    }

    #[test]
    fn keeps_the_text_untouched() {
        let u = ProviderUrl::parse("HTTPS://ChatGPT.com/A?b=C").unwrap();
        assert_eq!(u.as_str(), "HTTPS://ChatGPT.com/A?b=C");
    }
}
