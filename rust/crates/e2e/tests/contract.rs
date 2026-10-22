//! The command-line contract of the new client, exercised through the same sandbox as everything
//! else. These tests need no daemon, so they already run against the Rust client (Phase 1). On the
//! legacy launcher every invocation starts a GUI daemon, so they are skipped there; the behaviour
//! they pin down is the subject of the D-tests above.

use std::time::Duration;

use llm_wrapper_e2e::{skip, Sandbox};

macro_rules! rust_only {
    ($label:expr) => {{
        let Some(sb) = Sandbox::new_or_skip($label) else {
            return;
        };
        if sb.is_legacy() {
            skip!("legacy launcher starts a GUI daemon for any invocation (see D04, D05, D06)");
        }
        sb
    }};
}

#[test]
fn c01_help_and_version() {
    let sb = rust_only!("c01");
    let out = sb.run(&["--help"]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""));
    for needle in [
        "--toggle",
        "--provider NAME|URL",
        "--add-provider NAME URL",
        "--idle-timeout MINUTES",
        "--instance NAME",
        "--quit",
    ] {
        assert!(out.stdout.contains(needle), "help is missing {needle}");
    }
    assert!(sb.run(&["--version"]).stdout.starts_with("llm-wrapper "));
}

#[test]
fn c02_a_bad_command_line_fails_fast_with_exit_2_and_changes_nothing() {
    let sb = rust_only!("c02");
    let long = "A".repeat(6000);
    let cases: Vec<Vec<&str>> = vec![
        vec!["--toogle"],
        vec!["--tog"],
        vec!["stray"],
        vec!["--idle-timeout", "abc"],
        vec!["--idle-timeout", "99999999999"],
        vec!["--user-agent", &long],
        vec!["--user-agent", ""],
        vec!["--provider", "http://example.com"],
        vec!["--add-provider", "bad name", "https://x.example"],
        vec!["--add-provider", "ok", "javascript:alert(1)"],
        vec!["--add-provider", "ok", "https://user:pw@x.example"],
        vec!["--remove-provider", "bad name"],
        vec!["--instance", "bad name"],
        vec!["--toggle=1"],
    ];
    for args in cases {
        let out = sb.run(&args);
        assert_eq!(
            out.code,
            2,
            "{:?}: {}",
            &args[..args.len().min(2)],
            out.stderr
        );
        assert!(out.stderr.starts_with("llm-wrapper: "), "{}", out.stderr);
        assert!(
            out.elapsed < Duration::from_millis(500),
            "{:?}",
            out.elapsed
        );
    }
    assert!(!sb.socket_path(None).exists() && !sb.state_path(None).exists());
}

#[test]
fn c03_quitting_what_is_not_running_is_instant_and_silent() {
    let sb = rust_only!("c03");
    let out = sb.run(&["--quit"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    assert!(
        out.elapsed < Duration::from_millis(300),
        "{:?}: --quit must not start a GUI just to quit it",
        out.elapsed
    );
    assert!(!sb.state_path(None).exists());
}

#[test]
fn c04_a_world_accessible_runtime_directory_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let sb = rust_only!("c04");
    std::fs::set_permissions(&sb.runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = sb.run(&["--show"]);
    assert_eq!(out.code, 4, "{}", out.stderr);
    assert!(out.stderr.contains("0700"), "{}", out.stderr);
    std::fs::set_permissions(&sb.runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
}
