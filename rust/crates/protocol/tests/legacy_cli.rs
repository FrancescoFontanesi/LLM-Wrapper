//! Parity of the Rust command line with the legacy one.
//!
//! `fixtures/legacy_cli.json` is produced by `fixtures/gen_legacy_cli.py`, which lifts the real
//! `argparse` definition out of `app.py`. Each case says what the legacy app understood and what
//! the Rust parser is required to do. Intentional differences are listed in the fixture with a
//! reason code; this test fails on any *unexplained* difference, and on any explained one that no
//! longer differs.

use llm_wrapper_protocol::{
    parse_args, Flags, InstanceName, Parsed, ProviderName, ProviderRef, UserAgent,
};
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("fixtures/legacy_cli.json");

fn project(inv: &llm_wrapper_protocol::Invocation) -> Value {
    let r = &inv.request;
    let names = [
        (Flags::BACKGROUND, "background"),
        (Flags::HARD_CLEAN, "hard_clean"),
        (Flags::HIDE, "hide"),
        (Flags::NEW_CHAT, "new_chat"),
        (Flags::QUIT, "quit"),
        (Flags::SHOW, "show"),
        (Flags::SOFT_CLEAN, "soft_clean"),
        (Flags::TOGGLE, "toggle"),
    ];
    let flags: Vec<&str> = names
        .iter()
        .filter(|(f, _)| r.flags.contains(*f))
        .map(|(_, n)| *n)
        .collect();
    json!({
        "flags": flags,
        "idle_timeout": r.idle_timeout_minutes,
        "user_agent": r.user_agent.as_ref().map(UserAgent::as_str),
        "provider": r.provider.as_ref().map(|p| match p {
            ProviderRef::Name(n) => n.as_str().to_owned(),
            ProviderRef::Url(u) => u.as_str().to_owned(),
        }),
        "add_provider": r.add_provider.as_ref().map(|(n, u)| json!([n.as_str(), u.as_str()])),
        "remove_provider": r.remove_provider.as_ref().map(ProviderName::as_str),
        "instance": inv.instance.as_ref().map(InstanceName::as_str),
    })
}

#[test]
fn rust_cli_matches_legacy_except_where_declared() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 90, "fixture looks truncated");

    let mut failures = Vec::new();
    let mut deviations = std::collections::BTreeMap::<String, usize>::new();

    for case in cases {
        let args: Vec<String> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_owned())
            .collect();
        let expect = case["expect"].as_str().unwrap();
        let got = parse_args(&args);
        let ok = match (expect, &got) {
            ("same", Ok(Parsed::Run(inv))) => project(inv) == case["effective"],
            ("rejected", Err(_))
            | ("help", Ok(Parsed::Help))
            | ("version", Ok(Parsed::Version)) => true,
            (e, Err(_)) if e.starts_with("deviation:") => {
                *deviations.entry(e.to_owned()).or_default() += 1;
                true
            }
            _ => false,
        };
        if !ok {
            failures.push(format!("{args:?}: expected {expect}, got {got:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "unexplained differences from the legacy CLI:\n{}",
        failures.join("\n")
    );

    // Every declared deviation code is used, so the table cannot silently rot.
    for code in [
        "unknown_option",
        "abbreviation",
        "positional",
        "invalid_value",
        "url_policy",
        "strict_int",
        "range",
    ] {
        assert!(
            deviations.contains_key(&format!("deviation:{code}")),
            "no case exercises {code}"
        );
    }
    eprintln!("declared deviations exercised: {deviations:?}");
}

#[test]
fn legacy_facts_the_fixture_must_keep_recording() {
    // If these ever change the fixture was regenerated against a different legacy parser.
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let find = |args: &[&str]| {
        fixture["cases"].as_array().unwrap().iter().find(|c| {
            c["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap())
                .eq(args.iter().copied())
        })
    };
    // legacy swallows unknown flags and abbreviations
    assert_eq!(find(&["--toogle"]).unwrap()["legacy"], "parsed");
    assert_eq!(
        find(&["--tog"]).unwrap()["effective"]["flags"],
        json!(["toggle"])
    );
    // legacy turns a parse error into defaults (the window pops up); the fixture records it as rejected
    assert_eq!(
        find(&["--idle-timeout", "abc"]).unwrap()["legacy"],
        "rejected"
    );
    // legacy ignores an empty --user-agent
    assert_eq!(
        find(&["--user-agent", ""]).unwrap()["effective"]["user_agent"],
        Value::Null
    );
    // legacy clamps the idle timeout at one minute
    assert_eq!(
        find(&["--idle-timeout", "0"]).unwrap()["effective"]["idle_timeout"],
        1
    );
}
