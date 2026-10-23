//! What a link or a navigation may do: stay in the window, go to the default browser, or be refused.
//!
//! Pure functions over the URI text, so the whole policy is unit-tested without a display. The rule
//! is the legacy app's — a link stays in the window when its host is one of the domains the chat
//! sites need to log in, or belongs to the provider's own domain, and goes to the browser otherwise —
//! with the differences listed in `docs/spec.md` (V16..V18), each pinned by the fixture generated
//! from the legacy code (`tests/fixtures/gen_legacy_nav.py`).
//!
//! The text we are given is what `WebKit` puts in its request: already parsed and re-serialised by
//! its WHATWG URL parser (lower-case host, backslashes turned into slashes, escapes applied). We
//! still read it defensively, and anything we cannot read the same way twice is refused rather than
//! guessed at.

/// Hosts the app always treats as part of the chat experience: the sign-in providers, the bot
/// checks and the content networks the sites load. A link to one of them (or a subdomain) opens in
/// the window. The same list the legacy app has.
pub const INTERNAL_DOMAINS: &[&str] = &[
    "chatgpt.com",
    "openai.com",
    "auth0.openai.com",
    "auth.openai.com",
    "oaistatic.com",
    "oaiusercontent.com",
    "cloudflare.com",
    "challenges.cloudflare.com",
    "turnstile.com",
    "cloudflareinsights.com",
    "cf-assets.com",
    "arkoselabs.com",
    "livekit.cloud",
    "statsig.com",
    "statsigapi.net",
    "featuregates.org",
    "identrust.com",
    "sentry.io",
    "accounts.google.com",
    "appleid.apple.com",
    "login.microsoftonline.com",
    "login.live.com",
];

/// Longest URI we are willing to judge.
const MAX_URI_LEN: usize = 8 * 1024;

/// Second-level labels that, under a two-letter country code, are part of the public suffix
/// (`co.uk`, `com.au`, `or.jp`). A stand-in for the public suffix list, which would be a
/// dependency the size of the rest of the daemon.
const GENERIC_SECOND_LEVEL: &[&str] = &[
    "co", "com", "org", "net", "gov", "edu", "ac", "or", "ne", "go", "gob", "mil",
];

/// What to do with a link or a navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Load it in the window.
    Allow,
    /// Hand it to the default browser.
    OpenExternally,
    /// Do nothing.
    Block,
}

/// The parts of a URI the policy looks at.
#[derive(Debug, PartialEq, Eq)]
struct Parts {
    scheme: String,
    /// Lower-case host without brackets or a trailing dot; `None` for URIs without an authority.
    host: Option<String>,
    credentials: bool,
}

fn valid_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.contains(':') {
        // an IPv6 literal
        return host
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'));
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Read `uri`; `None` when it is not something we can judge.
fn parse(uri: &str) -> Option<Parts> {
    if uri.is_empty()
        || uri.len() > MAX_URI_LEN
        || uri
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace() || c == '\\' || !c.is_ascii())
    {
        return None;
    }
    let (scheme, rest) = uri.split_once(':')?;
    if !valid_scheme(scheme) {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let Some(after) = rest.strip_prefix("//") else {
        return Some(Parts {
            scheme,
            host: None,
            credentials: false,
        });
    };
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let (credentials, hostport) = match authority.rsplit_once('@') {
        Some((_, h)) => (true, h),
        None => (false, authority),
    };
    let host = if let Some(bracketed) = hostport.strip_prefix('[') {
        let (inside, tail) = bracketed.split_once(']')?;
        if !(tail.is_empty() || tail.strip_prefix(':').is_some_and(valid_port)) {
            return None;
        }
        inside
    } else {
        match hostport.rsplit_once(':') {
            Some((h, port)) if valid_port(port) => h,
            Some(_) => return None,
            None => hostport,
        }
    };
    // `chatgpt.com.` is the same host
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if !valid_host(&host) {
        return None;
    }
    Some(Parts {
        scheme,
        host: Some(host),
        credentials,
    })
}

fn valid_port(port: &str) -> bool {
    port.is_empty() || (port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()))
}

fn is_ipv4(host: &str) -> bool {
    let mut count = 0;
    for part in host.split('.') {
        count += 1;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    count == 4
}

fn is_ip(host: &str) -> bool {
    host.contains(':') || is_ipv4(host)
}

/// Is `host` this machine? (`localhost`, `127.0.0.0/8`, `::1`.)
pub fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || (is_ipv4(host) && host.split('.').next() == Some("127"))
}

/// The registrable part of a host: the last two labels, three under a country code with a generic
/// second level (`example.co.uk`). An IP address is its own base: sharing the last two numbers
/// says nothing about being the same site.
fn base_domain(host: &str) -> &str {
    if is_ip(host) {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host;
    }
    let keep = if labels[n - 1].len() == 2 && GENERIC_SECOND_LEVEL.contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    let skip: usize = labels[..n - keep].iter().map(|l| l.len() + 1).sum();
    &host[skip..]
}

fn same_or_below(host: &str, domain: &str) -> bool {
    host == domain
        || (!is_ip(domain)
            && host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// Does `host` belong to the chat experience of the provider at `provider_host`?
fn is_internal(host: &str, provider_host: Option<&str>) -> bool {
    INTERNAL_DOMAINS.iter().any(|d| same_or_below(host, d))
        || provider_host.is_some_and(|p| same_or_below(host, base_domain(p)))
}

/// The host of the provider URL, or `None` when it has none.
fn provider_host(provider_url: &str) -> Option<String> {
    parse(provider_url).and_then(|p| p.host)
}

/// Judge a link the user followed, a window a page opens, or a navigation the page starts, given
/// the provider currently on screen.
///
/// * `http`/`https` to a host that belongs to the provider or to the always-trusted list: stay in
///   the window — over `https`, or over `http` only to this machine; a cleartext link to a site goes
///   to the browser, which will upgrade it or warn.
/// * `http`/`https` anywhere else, and any address with a user name in it: the browser.
/// * `about:`, `blob:` and `data:` are what pages use inside themselves: allowed (`WebKit` refuses
///   a top-level `data:` by itself).
/// * Every other scheme — `file:`, `ftp:`, `mailto:`, `javascript:`, app-specific ones — is refused.
///   Nothing is ever launched for them: handing arbitrary schemes to the desktop is how a web page
///   starts programs.
pub fn classify(uri: &str, provider_url: &str) -> Verdict {
    let Some(parts) = parse(uri) else {
        return Verdict::Block;
    };
    match parts.scheme.as_str() {
        "http" | "https" => {
            let Some(host) = parts.host.as_deref() else {
                return Verdict::Block;
            };
            let provider = provider_host(provider_url);
            let stays = !parts.credentials
                && is_internal(host, provider.as_deref())
                && (parts.scheme == "https" || is_loopback(host));
            if stays {
                Verdict::Allow
            } else {
                Verdict::OpenExternally
            }
        }
        "about" | "blob" | "data" => Verdict::Allow,
        _ => Verdict::Block,
    }
}

/// May a navigation that is not a link the user followed (a redirect, a form, a frame, a script)
/// go ahead? Only the scheme matters here: a page may load third-party frames and follow redirects
/// anywhere on the web, but never to a local file or an application scheme.
pub fn scheme_allowed(uri: &str) -> bool {
    parse(uri).is_some_and(|p| {
        matches!(
            p.scheme.as_str(),
            "http" | "https" | "about" | "blob" | "data"
        )
    })
}

/// What is being decided: a navigation of a page, or a new window a page asks for.
#[derive(Debug)]
pub struct Navigation<'a> {
    /// A new window (`window.open`, `target=_blank`), as opposed to a navigation of the current one.
    pub new_window: bool,
    /// The user followed a link (as opposed to a redirect, a form, a script).
    pub link_clicked: bool,
    /// A click or a key press started it.
    pub user_gesture: bool,
    /// Where it goes, as `WebKit` parsed it.
    pub uri: &'a str,
}

/// What to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Let `WebKit` carry on.
    Proceed,
    /// Cancel it.
    Refuse,
    /// Cancel it and give the address to the default browser.
    OpenInBrowser,
}

/// The decision for one navigation or window request.
///
/// * Any navigation to a scheme other than the web's is refused, whoever starts it.
/// * A link the user clicked follows [`classify`].
/// * A new window follows [`classify`] too, but one that leaves for the browser needs a user
///   gesture: a page that opens windows on its own (`window.open` in a timer) gets nothing — the
///   same page can still open its own sign-in popups, which stay in the app.
/// * Anything else — redirects, frames, scripted navigation — goes ahead; the web view's own
///   protections apply to those.
pub fn decide(nav: &Navigation<'_>, provider_url: &str) -> Outcome {
    if nav.new_window {
        return match classify(nav.uri, provider_url) {
            Verdict::Allow => Outcome::Proceed,
            Verdict::OpenExternally if nav.user_gesture => Outcome::OpenInBrowser,
            Verdict::OpenExternally | Verdict::Block => Outcome::Refuse,
        };
    }
    if !scheme_allowed(nav.uri) {
        return Outcome::Refuse;
    }
    if nav.link_clicked && nav.user_gesture {
        return match classify(nav.uri, provider_url) {
            Verdict::Allow => Outcome::Proceed,
            Verdict::OpenExternally => Outcome::OpenInBrowser,
            Verdict::Block => Outcome::Refuse,
        };
    }
    Outcome::Proceed
}

#[cfg(test)]
mod tests {
    use super::*;

    const CG: &str = "https://chatgpt.com";

    fn v(uri: &str, provider: &str) -> Verdict {
        classify(uri, provider)
    }

    #[test]
    fn the_providers_own_site_and_the_login_domains_stay_in_the_window() {
        for uri in [
            "https://chatgpt.com/c/1",
            "https://cdn.chatgpt.com/a.js",
            "https://auth.openai.com/authorize",
            "https://accounts.google.com/signin",
            "https://challenges.cloudflare.com/turnstile/v0/api.js",
            "https://appleid.apple.com/auth/authorize",
        ] {
            assert_eq!(v(uri, CG), Verdict::Allow, "{uri}");
        }
    }

    #[test]
    fn other_sites_go_to_the_browser() {
        for uri in [
            "https://example.org/",
            "https://notchatgpt.com/",
            "https://chatgpt.com.evil.example/",
            "https://evil.example/chatgpt.com",
            "https://evil.example/?u=https://chatgpt.com",
            "https://[2001:db8::1]/",
        ] {
            assert_eq!(v(uri, CG), Verdict::OpenExternally, "{uri}");
        }
    }

    #[test]
    fn a_user_name_in_the_address_never_stays() {
        assert_eq!(
            v("https://chatgpt.com@evil.example/", CG),
            Verdict::OpenExternally
        );
        assert_eq!(
            v("https://user:pw@chatgpt.com/", CG),
            Verdict::OpenExternally
        );
    }

    #[test]
    fn cleartext_stays_only_on_this_machine() {
        assert_eq!(v("http://chatgpt.com/", CG), Verdict::OpenExternally);
        assert_eq!(
            v("http://localhost:3000/x", "http://localhost:3000"),
            Verdict::Allow
        );
        assert_eq!(
            v("http://127.0.0.1:9/", "http://127.0.0.1:8080"),
            Verdict::Allow
        );
        assert_eq!(v("http://[::1]:9/", "http://[::1]:8080"), Verdict::Allow);
    }

    #[test]
    fn base_domains() {
        assert_eq!(base_domain("chat.mistral.ai"), "mistral.ai");
        assert_eq!(base_domain("mistral.ai"), "mistral.ai");
        assert_eq!(base_domain("localhost"), "localhost");
        assert_eq!(base_domain("chat.example.co.uk"), "example.co.uk");
        assert_eq!(base_domain("a.b.c.example.com.au"), "example.com.au");
        assert_eq!(base_domain("co.uk"), "co.uk");
        assert_eq!(base_domain("a.b.example.com"), "example.com");
        assert_eq!(base_domain("127.0.0.1"), "127.0.0.1");
        assert_eq!(base_domain("2001:db8::1"), "2001:db8::1");
    }

    #[test]
    fn an_address_is_only_itself() {
        let p = "https://203.0.113.5";
        assert_eq!(v("https://203.0.113.5/x", p), Verdict::Allow);
        assert_eq!(v("https://9.9.113.5/", p), Verdict::OpenExternally);
        assert_eq!(v("https://x.203.0.113.5/", p), Verdict::OpenExternally);
    }

    #[test]
    fn a_public_suffix_is_not_one_site() {
        let p = "https://chat.example.co.uk";
        assert_eq!(v("https://login.example.co.uk/", p), Verdict::Allow);
        assert_eq!(v("https://attacker.co.uk/", p), Verdict::OpenExternally);
    }

    #[test]
    fn schemes_other_than_the_web_are_refused() {
        for uri in [
            "file:///etc/passwd",
            "FILE:///etc/passwd",
            "file:/etc/passwd",
            "file:etc/passwd",
            "FILE:/etc/passwd",
            "mailto:a@b.example",
            "tel:+123",
            "ftp://example.org/x",
            "javascript:alert(1)",
            "ms-msdt:/id",
            "intent://scan/#Intent;end",
            "view-source:https://chatgpt.com/",
        ] {
            assert_eq!(v(uri, CG), Verdict::Block, "{uri}");
            assert!(!scheme_allowed(uri), "{uri}");
        }
        for uri in [
            "about:blank",
            "blob:https://chatgpt.com/1-2",
            "data:text/plain,hi",
        ] {
            assert_eq!(v(uri, CG), Verdict::Allow, "{uri}");
            assert!(scheme_allowed(uri), "{uri}");
        }
        assert!(scheme_allowed("https://anything.example/") && scheme_allowed("http://x.example/"));
    }

    #[test]
    fn what_cannot_be_read_unambiguously_is_refused() {
        for uri in [
            "",
            "https://evil.example\\@chatgpt.com/",
            "https://chatgpt%2ecom/",
            "https:///x",
            "https://chatgpt.com/ x",
            "https://chatgp\u{74}.com\u{200b}/",
            "https://chatgpt.com:99999999/",
            "https://[::1/",
            "https://exa mple.org/",
            "://nohost",
            "1http://x.example/",
            "https://chatgpt.com/\n",
            "https://caf\u{e9}.example/",
            "https://chatgpt.com/caf\u{e9}",
            "https://chatgpt.com/?q=\u{2603}",
        ] {
            assert_eq!(v(uri, CG), Verdict::Block, "{uri:?}");
            assert!(!scheme_allowed(uri), "{uri:?}");
        }
        assert_eq!(
            v(&format!("https://x.example/{}", "a".repeat(9000)), CG),
            Verdict::Block
        );
    }

    #[test]
    fn a_trailing_dot_and_the_case_of_the_host_do_not_matter() {
        assert_eq!(v("https://chatgpt.com./", CG), Verdict::Allow);
        assert_eq!(v("https://CHATGPT.COM/", CG), Verdict::Allow);
        assert_eq!(v("HTTPS://chatgpt.com:8443/", CG), Verdict::Allow);
    }

    #[test]
    fn without_a_usable_provider_only_the_fixed_list_counts() {
        assert_eq!(v("https://chatgpt.com/", "not a url"), Verdict::Allow);
        assert_eq!(
            v("https://claude.ai/", "not a url"),
            Verdict::OpenExternally
        );
    }

    #[test]
    fn parsing_pulls_out_the_right_host() {
        let p = parse("https://user@[2001:DB8::1]:8443/path?q#f").unwrap();
        assert_eq!(p.host.as_deref(), Some("2001:db8::1"));
        assert!(p.credentials);
        assert_eq!(parse("mailto:a@b.example").unwrap().host, None);
        assert_eq!(
            parse("https://a.example:/x").unwrap().host.as_deref(),
            Some("a.example")
        );
    }

    /// The legacy rule, run through the code lifted from `app.py` (see the generator): every case
    /// either agrees with what the legacy app does or is a documented deviation with the verdict
    /// Rust must give.
    #[test]
    fn agrees_with_the_legacy_app_except_where_deliberately_different() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/legacy_nav.json")).unwrap();
        // the fixture's domain list is the legacy one: ours must be identical
        let mut legacy: Vec<&str> = fixture["allowed_domains"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d.as_str().unwrap())
            .collect();
        let mut ours = INTERNAL_DOMAINS.to_vec();
        legacy.sort_unstable();
        ours.sort_unstable();
        assert_eq!(
            ours, legacy,
            "the always-trusted domains drifted from app.py"
        );

        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() > 40);
        let (mut same, mut deviations) = (0, 0);
        for case in cases {
            let uri = case["uri"].as_str().unwrap();
            let provider = case["provider"].as_str().unwrap();
            let got = classify(uri, provider);
            let expect = case["expect"].as_str().unwrap();
            if expect == "same" {
                let want = if case["legacy_external"].as_bool().unwrap() {
                    Verdict::OpenExternally
                } else {
                    Verdict::Allow
                };
                assert_eq!(
                    got, want,
                    "{uri} (provider {provider}): legacy does {want:?}"
                );
                same += 1;
            } else {
                assert!(expect.starts_with("deviation:"), "{expect}");
                let want = match case["rust"].as_str().unwrap() {
                    "allow" => Verdict::Allow,
                    "external" => Verdict::OpenExternally,
                    "block" => Verdict::Block,
                    other => panic!("{other}"),
                };
                assert_eq!(got, want, "{uri} (provider {provider}): {expect}");
                deviations += 1;
            }
        }
        assert!(
            same > 30 && deviations > 15,
            "{same} same, {deviations} deviations"
        );
    }

    fn nav(new_window: bool, link_clicked: bool, user_gesture: bool, uri: &str) -> Outcome {
        decide(
            &Navigation {
                new_window,
                link_clicked,
                user_gesture,
                uri,
            },
            CG,
        )
    }

    #[test]
    fn a_clicked_link_follows_the_classification() {
        assert_eq!(
            nav(false, true, true, "https://chatgpt.com/c/1"),
            Outcome::Proceed
        );
        assert_eq!(
            nav(false, true, true, "https://example.org/"),
            Outcome::OpenInBrowser
        );
        assert_eq!(
            nav(false, true, true, "mailto:a@b.example"),
            Outcome::Refuse
        );
    }

    #[test]
    fn navigations_the_user_did_not_click_are_only_checked_for_their_scheme() {
        for gesture in [false, true] {
            assert_eq!(
                nav(false, false, gesture, "https://example.org/x"),
                Outcome::Proceed
            );
            assert_eq!(
                nav(false, false, gesture, "https://challenges.cloudflare.com/"),
                Outcome::Proceed
            );
            assert_eq!(
                nav(false, false, gesture, "file:///etc/passwd"),
                Outcome::Refuse
            );
            assert_eq!(
                nav(false, false, gesture, "intent://x#Intent;end"),
                Outcome::Refuse
            );
        }
        // a link without a gesture (a script clicking it) is not a link the user followed
        assert_eq!(
            nav(false, true, false, "https://example.org/"),
            Outcome::Proceed
        );
        assert_eq!(
            nav(false, true, false, "file:///etc/passwd"),
            Outcome::Refuse
        );
    }

    #[test]
    fn a_new_window_stays_for_sign_in_and_leaves_only_on_a_click() {
        for gesture in [false, true] {
            assert_eq!(
                nav(true, false, gesture, "https://auth.openai.com/x"),
                Outcome::Proceed
            );
            assert_eq!(nav(true, false, gesture, "about:blank"), Outcome::Proceed);
            assert_eq!(
                nav(true, false, gesture, "file:///etc/passwd"),
                Outcome::Refuse
            );
            assert_eq!(nav(true, false, gesture, "https:///"), Outcome::Refuse);
        }
        assert_eq!(
            nav(true, false, true, "https://example.org/"),
            Outcome::OpenInBrowser
        );
        assert_eq!(
            nav(true, false, false, "https://example.org/"),
            Outcome::Refuse
        );
    }
}
