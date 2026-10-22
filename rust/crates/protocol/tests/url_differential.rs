//! Differential test against an independent WHATWG URL parser (the `url` crate, a dev-dependency
//! only). The validator exists to guarantee that *what we accepted means the same thing to every
//! parser downstream* (`GLib`, `WebKit`). So: whatever `ProviderUrl` accepts, the reference parser must
//! accept and read identically, and a host we call a DNS name must never be an IP address to it.

mod common;

use common::Rng;
use llm_wrapper_protocol::ProviderUrl;
use url::{Host, Url};

fn check_agrees(input: &str) {
    let ours =
        ProviderUrl::parse(input).unwrap_or_else(|e| panic!("{input:?} should be accepted: {e}"));
    let theirs = Url::parse(ours.as_str())
        .unwrap_or_else(|e| panic!("reference parser rejects what we accepted {input:?}: {e}"));

    assert!(matches!(theirs.scheme(), "http" | "https"), "{input:?}");
    assert_eq!(
        theirs.username(),
        "",
        "{input:?}: credentials slipped through"
    );
    assert_eq!(theirs.password(), None, "{input:?}");

    // authority of the original, to compare the host *text* with what the reference parser decided
    let after = input.split_once("://").unwrap().1;
    let authority = &after[..after.find(['/', '?', '#']).unwrap_or(after.len())];
    match theirs.host().expect("host") {
        Host::Domain(d) => {
            assert!(!authority.starts_with('['), "{input:?}");
            let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
            assert!(
                host.eq_ignore_ascii_case(d),
                "{input:?}: host {host} vs {d}"
            );
            // the crux: a "DNS name" for us must not be a numeric address for them
            assert!(
                !d.bytes().last().is_some_and(|b| b.is_ascii_digit())
                    || d.rsplit('.')
                        .next()
                        .unwrap()
                        .bytes()
                        .any(|b| !b.is_ascii_digit()),
                "{input:?}: numeric-looking domain"
            );
        }
        Host::Ipv4(ip) => {
            let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
            assert_eq!(
                host,
                ip.to_string(),
                "{input:?}: IPv4 was not in canonical dotted-quad form"
            );
        }
        Host::Ipv6(ip) => {
            assert!(authority.starts_with('['), "{input:?}");
            let inner = &authority[1..authority.find(']').unwrap()];
            assert_eq!(
                inner.parse::<std::net::Ipv6Addr>().unwrap(),
                ip,
                "{input:?}"
            );
        }
    }
    // port
    let port_text = if authority.starts_with('[') {
        authority.rsplit_once("]:").map(|(_, p)| p)
    } else {
        authority.rsplit_once(':').map(|(_, p)| p)
    };
    if let Some(p) = port_text {
        assert_eq!(
            theirs.port_or_known_default(),
            Some(p.parse().unwrap()),
            "{input:?}"
        );
    }
    // http is loopback-only, and "loopback" must mean loopback to the reference parser too
    if theirs.scheme() == "http" {
        let loopback = match theirs.host().unwrap() {
            Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
            Host::Ipv4(ip) => ip.is_loopback(),
            Host::Ipv6(ip) => ip.is_loopback(),
        };
        assert!(
            loopback,
            "{input:?}: cleartext http accepted for a non-loopback host"
        );
    }
}

#[test]
fn curated_corpus_agrees() {
    for ok in [
        "https://chatgpt.com",
        "https://chatgpt.com/",
        "HTTPS://ChatGPT.com/a?b=c#d",
        "https://gemini.google.com/app",
        "https://claude.ai",
        "https://chat.mistral.ai",
        "https://x.example:8443/p",
        "https://1.2.3.4:5/",
        "https://[2001:db8::1]:443/",
        "https://[::1]/",
        "http://localhost",
        "http://localhost:3000/x",
        "http://127.0.0.1:8080",
        "http://127.9.9.9/",
        "http://[::1]:1/",
        "https://a-b.c-d.example/%41%2f",
    ] {
        check_agrees(ok);
    }
}

/// Inputs that a lenient parser reads as 127.0.0.1 or as credentials; we must refuse every one.
#[test]
fn known_parser_differential_tricks_are_refused() {
    for evil in [
        "https://2130706433/",
        "https://0x7f000001/",
        "https://0x7f.0.0.1/",
        "https://127.1/",
        "https://017700000001/",
        "https://127.0.0.1.example.com@evil.com/",
        "https://example.com\\@evil.com/",
        "https://evil.com\\.example.com/",
        "https://example.com%00.evil.com/",
        "https://example.com%2f@evil.com/",
        "https://\u{ff45}vil.com/",
        "https://[::1]@evil.com/",
        "http://localhost@evil.com/",
        "http://example.com#@localhost/",
        "http://localhost.evil.com/",
        "https:/\\example.com/",
        "https:example.com",
        "https://example.com:80:80/",
        "https://example.com:+80/",
        "http://0177.0.0.1/",
        "http://127.0.0.1.nip.io/",
        "http://[::ffff:7f00:1]/",
        "http://[0:0:0:0:0:0:0:1%25lo]/",
    ] {
        assert!(
            ProviderUrl::parse(evil).is_err(),
            "{evil:?} must be refused"
        );
    }
}

#[test]
fn random_urls_never_disagree_with_the_reference_parser() {
    let mut rng = Rng::new(0x5eed_0001);
    let schemes = ["https", "http", "HTTPS", "Http", "ftp", ""];
    let host_chars = "abcdefghijklmnopqrstuvwxyz0123456789-.xX";
    let ports = [
        "", ":443", ":80", ":8080", ":0", ":65535", ":65536", ":abc", ":",
    ];
    let tails = [
        "", "/", "/a/b", "/a?b=c", "?q", "#f", "/%41", "/%4", "/a b", "/\\", "/%zz", "/é",
    ];
    let mut accepted = 0_u32;
    let mut accepted_http = 0_u32;

    for _ in 0..60_000 {
        let host = match rng.below(8) {
            0 => format!(
                "{}.{}.{}.{}",
                rng.below(300),
                rng.below(300),
                rng.below(300),
                rng.below(300)
            ),
            1 => "localhost".to_owned(),
            2 => format!(
                "{}.{}.{}.{}",
                127,
                rng.below(256),
                rng.below(256),
                rng.below(256)
            ),
            3 => format!("[{}]", rng.string_from("0123456789abcdef:", 1, 12)),
            4 => format!("{}", rng.next_u64() % 5_000_000_000),
            5 => format!("0x{}", rng.string_from("0123456789abcdefg", 1, 8)),
            _ => rng.string_from(host_chars, 1, 18),
        };
        let input = format!(
            "{}://{}{}{}",
            rng.pick(&schemes),
            host,
            rng.pick(&ports),
            rng.pick(&tails)
        );
        if ProviderUrl::parse(&input).is_ok() {
            accepted += 1;
            if input.to_ascii_lowercase().starts_with("http:") {
                accepted_http += 1;
            }
            check_agrees(&input);
        }
    }
    // the generator must actually exercise acceptance, otherwise this test proves nothing
    assert!(accepted > 2_000, "only {accepted} accepted inputs");
    assert!(
        accepted_http > 50,
        "only {accepted_http} accepted http inputs"
    );
    eprintln!("random URL differential: {accepted} accepted ({accepted_http} http) of 60000, no disagreement");
}
