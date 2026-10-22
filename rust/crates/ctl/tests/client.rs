//! The real `llm-wrapper` binary against a protocol-speaking mock daemon.

mod common;

use std::os::unix::ffi::OsStringExt;
use std::time::Duration;

use common::{run_binary_os, run_client, Behavior, MockDaemon, TestDir, CLIENT};
use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::{Flags, ProviderRef, Response, Status};

fn daemon(dir: &TestDir, b: Behavior) -> MockDaemon {
    MockDaemon::start(dir.socket(None), b)
}

#[test]
fn help_and_version_need_no_daemon_and_no_runtime_dir() {
    let out = run_binary_os(CLIENT, None, vec!["--help".into()], &[]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""));
    assert!(
        out.stdout.contains("Usage: llm-wrapper") && out.stdout.contains("--add-provider NAME URL")
    );
    let out = run_binary_os(
        CLIENT,
        Some(std::path::Path::new("/nonexistent")),
        vec!["-V".into()],
        &[],
    );
    assert_eq!(out.code, 0);
    assert!(out.stdout.starts_with("llm-wrapper 0."), "{}", out.stdout);
}

#[test]
fn sends_exactly_the_typed_request() {
    let dir = TestDir::new("typed");
    let d = daemon(&dir, Behavior::Reply(Response::ok()));
    let out = run_client(
        dir.path(),
        &[
            "--provider",
            "gemini",
            "--idle-timeout",
            "20",
            "--show",
            "--user-agent",
            "UA/9",
        ],
    );
    assert_eq!(
        (out.code, out.stderr.as_str(), out.stdout.as_str()),
        (0, "", "")
    );
    let req = d.next_request().unwrap();
    assert!(req.flags.contains(Flags::SHOW) && !req.flags.contains(Flags::TOGGLE));
    assert_eq!(req.idle_timeout_minutes, Some(20));
    assert_eq!(req.user_agent.unwrap().as_str(), "UA/9");
    assert!(matches!(req.provider, Some(ProviderRef::Name(n)) if n.as_str() == "gemini"));
}

#[test]
fn bare_invocation_toggles() {
    let dir = TestDir::new("bare");
    let d = daemon(&dir, Behavior::Reply(Response::ok()));
    assert_eq!(run_client(dir.path(), &[]).code, 0);
    let req = d.next_request().unwrap();
    assert!(req.flags.contains(Flags::TOGGLE));
}

#[test]
fn instance_selects_the_socket() {
    let dir = TestDir::new("inst");
    let default = daemon(&dir, Behavior::Reply(Response::ok()));
    let named = MockDaemon::start(dir.socket(Some("work")), Behavior::Reply(Response::ok()));
    assert_eq!(
        run_client(dir.path(), &["--instance", "work", "--hide"]).code,
        0
    );
    assert!(named.next_request().unwrap().flags.contains(Flags::HIDE));
    assert!(default.no_request_within(Duration::from_millis(200)));
}

#[test]
fn usage_errors_exit_2_without_touching_the_daemon() {
    let dir = TestDir::new("usage");
    let d = daemon(&dir, Behavior::Reply(Response::ok()));
    for args in [
        &["--bogus"][..],
        &["--tog"],
        &["--idle-timeout", "abc"],
        &["--provider", "http://example.com"],
        &["stray"],
    ] {
        let out = run_client(dir.path(), args);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert!(out.stderr.starts_with("llm-wrapper: "), "{}", out.stderr);
        assert!(out.stdout.is_empty());
    }
    assert!(
        d.no_request_within(Duration::from_millis(200)),
        "a rejected command line reached the daemon"
    );
    let out = run_client(dir.path(), &["--tog"]);
    assert!(
        out.stderr.contains("did you mean '--toggle'"),
        "{}",
        out.stderr
    );
}

#[test]
fn non_utf8_and_oversized_arguments_are_usage_errors() {
    let dir = TestDir::new("args");
    let d = daemon(&dir, Behavior::Reply(Response::ok()));
    let out = run_binary_os(
        CLIENT,
        Some(dir.path()),
        vec![std::ffi::OsString::from_vec(vec![b'-', b'-', 0xff, 0xfe])],
        &[],
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
    let big = "x".repeat(100_000);
    let out = run_client(dir.path(), &["--user-agent", &big]);
    assert_eq!(out.code, 2);
    assert!(d.no_request_within(Duration::from_millis(150)));
}

#[test]
fn daemon_refusal_is_reported_with_exit_1() {
    let dir = TestDir::new("refuse");
    let _d = daemon(
        &dir,
        Behavior::Reply(Response::with_message(
            Status::Rejected,
            "unknown provider: nope",
        )),
    );
    let out = run_client(dir.path(), &["--provider", "nope"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "llm-wrapper: unknown provider: nope\n");
}

#[test]
fn daemon_text_cannot_rewrite_the_terminal() {
    let dir = TestDir::new("ansi");
    let _d = daemon(
        &dir,
        Behavior::Reply(Response::with_message(
            Status::Rejected,
            "\u{1b}[2J\u{1b}]0;pwned\u{7}evil\u{202e}txt\r",
        )),
    );
    let out = run_client(dir.path(), &["--show"]);
    assert_eq!(out.code, 1);
    assert!(
        !out.stderr
            .bytes()
            .any(|b| b == 0x1b || b == 0x07 || b == b'\r'),
        "{:?}",
        out.stderr
    );
    assert!(!out.stderr.contains('\u{202e}'));
}

#[test]
fn every_failure_status_maps_to_a_distinct_documented_code() {
    for (status, code) in [
        (Status::Rejected, 1),
        (Status::Busy, 1),
        (Status::Internal, 1),
        (Status::Unsupported, 3),
    ] {
        let dir = TestDir::new("status");
        let _d = daemon(&dir, Behavior::Reply(Response::with_message(status, "x")));
        assert_eq!(run_client(dir.path(), &["--show"]).code, code, "{status:?}");
    }
}

#[test]
fn malformed_replies_exit_3_and_say_why() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("legacy text reply", b"OK\n".to_vec()),
        ("empty reply", vec![]),
        ("truncated header", b"LW\x01".to_vec()),
        ("wrong version", b"LW\x02\x02\x00\x01\x00".to_vec()),
        (
            "request kind echoed back",
            b"LW\x01\x01\x00\x01\x00".to_vec(),
        ),
        (
            "length above the response limit",
            b"LW\x01\x02\x1f\x40".to_vec(),
        ),
        (
            "body shorter than declared",
            b"LW\x01\x02\x00\x05\x00".to_vec(),
        ),
        ("unknown status", b"LW\x01\x02\x00\x01\x09".to_vec()),
        (
            "invalid utf-8 message",
            b"LW\x01\x02\x00\x03\x00\xff\xfe".to_vec(),
        ),
    ];
    for (label, bytes) in cases {
        let dir = TestDir::new("badreply");
        let _d = daemon(&dir, Behavior::Raw(bytes));
        let out = run_client(dir.path(), &["--show"]);
        assert_eq!(out.code, 3, "{label}: {}", out.stderr);
        assert!(
            out.stderr.contains("unexpected reply") || out.stderr.contains("did not answer"),
            "{label}: {}",
            out.stderr
        );
        assert!(
            out.elapsed < Duration::from_millis(1800),
            "{label}: {:?}",
            out.elapsed
        );
    }
}

#[test]
fn a_different_protocol_version_gets_actionable_advice() {
    let dir = TestDir::new("ver");
    let _d = daemon(&dir, Behavior::Raw(b"LW\x02\x02\x00\x01\x00".to_vec()));
    let out = run_client(dir.path(), &["--show"]);
    assert_eq!(out.code, 3);
    assert!(
        out.stderr.contains("unsupported protocol version 2") && out.stderr.contains("--quit"),
        "{}",
        out.stderr
    );
}

#[test]
fn a_silent_daemon_cannot_hang_the_hotkey() {
    let dir = TestDir::new("silent");
    let _d = daemon(&dir, Behavior::Silent(Duration::from_secs(10)));
    let out = run_client(dir.path(), &["--toggle"]);
    assert_eq!(out.code, 3);
    assert!(out.stderr.contains("did not answer"), "{}", out.stderr);
    assert!(
        out.elapsed > Duration::from_millis(600) && out.elapsed < Duration::from_millis(1800),
        "{:?}",
        out.elapsed
    );
}

#[test]
fn a_trickling_daemon_is_bounded_by_the_total_deadline_not_per_byte() {
    let dir = TestDir::new("trickle");
    let _d = daemon(&dir, Behavior::Trickle(Duration::from_millis(300)));
    let out = run_client(dir.path(), &["--toggle"]);
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        out.elapsed < Duration::from_millis(1800),
        "{:?}",
        out.elapsed
    );
}

#[test]
fn a_daemon_that_hangs_up_is_an_error_not_success() {
    let dir = TestDir::new("hangup");
    let _d = daemon(&dir, Behavior::CloseImmediately);
    let out = run_client(dir.path(), &["--toggle"]);
    assert_eq!(out.code, 3, "{}", out.stderr);
}

#[test]
fn a_flooding_daemon_is_not_read_beyond_the_header() {
    let dir = TestDir::new("flood");
    // header declares a 60000 byte response, far above the 1 KiB limit, then floods
    let d = daemon(&dir, Behavior::Flood(b"LW\x01\x02\xea\x60".to_vec()));
    let out = run_client(dir.path(), &["--toggle"]);
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        out.elapsed < Duration::from_millis(1500),
        "{:?}",
        out.elapsed
    );
    assert!(*d.flood_sent.lock().unwrap() < 8 * 1024 * 1024);
}

#[test]
fn a_reply_may_be_followed_by_junk_without_harm() {
    let dir = TestDir::new("junk");
    let mut bytes = Response::ok().encode().unwrap();
    bytes.extend_from_slice(&[b'Z'; 10_000]);
    let _d = daemon(&dir, Behavior::Raw(bytes));
    assert_eq!(run_client(dir.path(), &["--show"]).code, 0);
}

#[test]
fn nothing_running_and_quit_is_a_fast_no_op() {
    let dir = TestDir::new("quit");
    let out = run_client(dir.path(), &["--quit"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    assert!(
        out.elapsed < Duration::from_millis(200),
        "{:?}",
        out.elapsed
    );
}

#[test]
fn quit_reaches_a_running_daemon() {
    let dir = TestDir::new("quit2");
    let d = daemon(&dir, Behavior::Reply(Response::ok()));
    assert_eq!(run_client(dir.path(), &["--quit"]).code, 0);
    assert!(d.next_request().unwrap().flags.contains(Flags::QUIT));
}

#[test]
fn a_stale_socket_counts_as_no_daemon() {
    let dir = TestDir::new("stale");
    {
        let _l = std::os::unix::net::UnixListener::bind(dir.socket(None)).unwrap();
        std::fs::set_permissions(
            dir.socket(None),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
    } // listener dropped, socket file stays: ECONNREFUSED
    assert!(dir.socket(None).exists());
    assert_eq!(run_client(dir.path(), &["--quit"]).code, 0);
}

#[test]
fn the_legacy_socket_is_never_spoken_to() {
    let dir = TestDir::new("legacy");
    let legacy =
        std::os::unix::net::UnixListener::bind(dir.path().join("llm-wrapper.sock")).unwrap();
    legacy.set_nonblocking(true).unwrap();
    let out = run_client(dir.path(), &["--show"]); // no v1 daemon: tries to start one, finds none installed
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        legacy.accept().is_err(),
        "the client connected to the legacy socket"
    );
}

#[test]
fn many_concurrent_clients_all_succeed() {
    let dir = TestDir::new("many");
    let d = daemon(&dir, Behavior::Reply(Response::ok()));
    let path = dir.path().to_owned();
    let handles: Vec<_> = (0..40)
        .map(|_| {
            let p = path.clone();
            std::thread::spawn(move || run_client(&p, &["--show"]).code)
        })
        .collect();
    let ok = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|c| *c == 0)
        .count();
    assert_eq!(ok, 40);
    for _ in 0..40 {
        d.next_request().unwrap();
    }
}

#[test]
fn golden_frame_on_the_wire_is_what_the_docs_say() {
    // the daemon double decodes with production code; here we also pin the raw bytes
    let dir = TestDir::new("raw");
    let listener = std::os::unix::net::UnixListener::bind(dir.socket(None)).unwrap();
    std::fs::set_permissions(
        dir.socket(None),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let handle = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut buf = vec![0_u8; 11];
        std::io::Read::read_exact(&mut c, &mut buf).unwrap();
        std::io::Write::write_all(&mut c, &Response::ok().encode().unwrap()).unwrap();
        buf
    });
    assert_eq!(run_client(dir.path(), &["--toggle"]).code, 0);
    assert_eq!(
        handle.join().unwrap(),
        b"LW\x01\x01\x00\x05\x01\x00\x02\x00\x01"
    );
    let _ = frame::encode(Kind::Request, &[]); // keep the import honest
}
