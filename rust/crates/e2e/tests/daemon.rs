//! What only the Rust daemon promises ("S" tests): it survives hostile or broken peers, there is
//! never more than one of it per instance, everything it writes is private, and it neither follows
//! planted symlinks nor trusts permissive directories. Skipped on the legacy app, which has none of
//! these properties (see D07, D10 for the ones that are also defects there).

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use llm_wrapper_e2e::proc;
use llm_wrapper_e2e::{skip, skip_by_design, FakeChat, Sandbox};
use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::{Response, Status};

const SECS: fn(u64) -> Duration = Duration::from_secs;

macro_rules! rust_daemon {
    ($label:expr) => {{
        let Some(sb) = Sandbox::new_or_skip($label) else {
            return;
        };
        if sb.is_legacy() {
            skip_by_design!("Rust daemon only");
        }
        if !sb.has_daemon() {
            skip!("no Rust daemon built, or no display to open its window");
        }
        sb
    }};
}

fn mode(path: &std::path::Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn open_fds(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd")).unwrap().count()
}

/// Read until the peer closes or `limit` passes; returns what arrived and whether it closed.
fn drain(stream: &mut UnixStream, limit: Duration) -> (Vec<u8>, bool) {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let start = Instant::now();
    let mut got = Vec::new();
    let mut buf = [0_u8; 256];
    while start.elapsed() < limit {
        match stream.read(&mut buf) {
            Ok(0) => return (got, true),
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return (got, true), // reset by peer
        }
    }
    (got, false)
}

fn reply_status(bytes: &[u8]) -> Status {
    let body = frame::read(&mut &bytes[..], Kind::Response).expect("a well-formed response frame");
    Response::decode(&body).unwrap().status
}

fn connect(sb: &Sandbox) -> UnixStream {
    UnixStream::connect(sb.socket_path(None)).expect("connect to the daemon")
}

fn assert_still_serving(sb: &Sandbox) {
    let out = sb.run(&["--show"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    sb.wait_state(None, "window visible", |s| s["visible"] == true);
}

#[test]
fn s01_hostile_peers_do_not_hurt_the_daemon() {
    let mut sb = rust_daemon!("s01");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    let pids = sb.daemon_pids();
    assert_eq!(pids.len(), 1);

    // not our protocol: closed without a word
    let mut c = connect(&sb);
    c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let (got, closed) = drain(&mut c, SECS(3));
    assert!(closed && got.is_empty(), "answered garbage with {got:?}");

    // a protocol version it does not speak: told so
    let mut c = connect(&sb);
    c.write_all(&[b'L', b'W', 9, 1, 0, 0]).unwrap();
    let (got, _) = drain(&mut c, SECS(3));
    assert_eq!(reply_status(&got), Status::Unsupported);

    // a body larger than the protocol allows: refused before it is read
    let mut c = connect(&sb);
    c.write_all(&[b'L', b'W', 1, 1, 0xEA, 0x60]).unwrap();
    let (got, closed) = drain(&mut c, SECS(3));
    assert!(closed);
    assert_ne!(reply_status(&got), Status::Ok);

    // a well-formed frame around nonsense
    let mut c = connect(&sb);
    c.write_all(&frame::encode(Kind::Request, &[0xFF, 0xFF, 0xFF, 0xFF]).unwrap())
        .unwrap();
    let (got, _) = drain(&mut c, SECS(3));
    assert_eq!(reply_status(&got), Status::Rejected);

    // a request that stops half way: dropped at the deadline, not held open
    let mut c = connect(&sb);
    c.write_all(&[b'L', b'W', 1, 1, 0, 10, 1, 2]).unwrap();
    let start = Instant::now();
    let (_, closed) = drain(&mut c, SECS(5));
    assert!(closed, "a stalled request was never dropped");
    assert!(
        start.elapsed() < SECS(3),
        "dropped after {:?}",
        start.elapsed()
    );

    assert_eq!(sb.daemon_pids(), pids, "the daemon was replaced or died");
    assert_still_serving(&sb);
}

#[test]
fn s02_idle_connections_cost_nothing_lasting() {
    let mut sb = rust_daemon!("s02");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    let pid = sb.daemon_pids()[0];
    // one full show/hide cycle first, so lazily created GTK/WebKit resources are not counted
    assert_still_serving(&sb);
    assert_eq!(sb.run(&["--hide"]).code, 0);
    sb.wait_state(None, "window hidden", |s| s["visible"] == false);
    thread::sleep(Duration::from_millis(500));
    let before = open_fds(pid);

    let idle: Vec<UnixStream> = (0..200)
        .filter_map(|_| UnixStream::connect(sb.socket_path(None)).ok())
        .collect();
    assert!(
        idle.len() > 100,
        "only {} connections were accepted",
        idle.len()
    );
    assert!(proc::alive(pid));
    drop(idle);

    // whatever happened during the burst, the daemon recovers by itself and holds nothing on to
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        sb.run(&["--show"]).code,
        0,
        "no command is accepted after the burst"
    );
    assert_eq!(sb.run(&["--hide"]).code, 0);
    sb.wait_state(None, "window hidden", |s| s["visible"] == false);
    thread::sleep(Duration::from_millis(500));
    // A leak would cost one descriptor per connection (200 here); the toolkit may legitimately open
    // or close a few shared-memory buffers of its own between two measurements.
    assert!(
        open_fds(pid) <= before + 8,
        "file descriptors grew from {before} to {}",
        open_fds(pid)
    );
    assert_eq!(sb.daemon_pids(), vec![pid]);
}

#[test]
fn s03_simultaneous_starts_leave_exactly_one_daemon() {
    let mut sb = rust_daemon!("s03");
    let chat = FakeChat::start();
    let url = chat.url();
    let n = 5;
    for i in 0..n {
        // the commands differ so that a lost one would be noticed
        let extra = if i == 0 { "--show" } else { "--hide" };
        sb.start(&["--provider", &url, extra]);
    }
    sb.wait_state(None, "the daemon to report the provider", |s| {
        s["url"] == url.as_str()
    });
    let deadline = Instant::now() + SECS(20);
    loop {
        let finished = (0..n).filter(|i| sb.daemon_exited(*i)).count();
        if finished == n - 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{finished} of {n} launchers finished; the rest should have handed over and exited"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(sb.daemon_pids().len(), 1, "more than one daemon is running");
    for i in 0..n {
        if let Some(code) = sb.daemon_exit_code(i) {
            assert_eq!(
                code,
                0,
                "launcher {i} that handed over failed: {}",
                sb.daemon_log(i)
            );
        }
    }
}

#[test]
fn s04_everything_the_daemon_creates_is_private() {
    let mut sb = rust_daemon!("s04");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    assert_eq!(
        sb.run(&["--add-provider", "extra", "https://extra.example"])
            .code,
        0
    );
    sb.wait_state(None, "provider added", |s| {
        s["providers"]["extra"].is_string()
    });

    let runtime = sb.socket_path(None).parent().unwrap().to_path_buf();
    assert_eq!(mode(&runtime), 0o700, "runtime directory");
    assert_eq!(mode(&sb.socket_path(None)), 0o600, "socket");
    assert_eq!(mode(&sb.state_path(None)), 0o600, "state file");
    assert_eq!(mode(&runtime.join("llm-wrapper.lock")), 0o600, "lock file");
    for dir in sb.data_dirs() {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
    assert_eq!(mode(&sb.providers_file()), 0o600, "provider list");
    // nothing temporary is left behind by the atomic writes
    for dir in [
        runtime.clone(),
        sb.providers_file().parent().unwrap().to_path_buf(),
    ] {
        let stray: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(stray.is_empty(), "{}: {stray:?}", dir.display());
    }
}

#[test]
fn s05_a_symlinked_provider_list_is_not_followed() {
    let mut sb = rust_daemon!("s05");
    let chat = FakeChat::start();
    let file = sb.providers_file();
    let dir = file.parent().unwrap();
    fs::create_dir_all(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let decoy = sb.providers_file().with_file_name("decoy.json");
    fs::write(&decoy, r#"{"planted":"https://planted.example"}"#).unwrap();
    symlink(&decoy, &file).unwrap();

    sb.start_hidden(&chat.url());
    let state = sb.state(None).unwrap();
    assert!(
        state["providers"].get("planted").is_none(),
        "followed the link: {state}"
    );
    assert!(
        state["providers"].get("chatgpt").is_some(),
        "built-in list expected: {state}"
    );
    // what was behind the link is untouched, and the link itself was kept aside, not erased
    assert_eq!(
        fs::read_to_string(&decoy).unwrap(),
        r#"{"planted":"https://planted.example"}"#
    );
    let moved = fs::read_dir(dir).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with("providers.json.untrusted")
    });
    assert!(moved, "the symlink was not set aside");
    assert!(
        sb.daemon_log(0).contains("not used"),
        "the log should say why: {}",
        sb.daemon_log(0)
    );
    // saving later writes a regular file, never through the old link
    assert_eq!(
        sb.run(&["--add-provider", "extra", "https://extra.example"])
            .code,
        0
    );
    sb.wait_state(None, "provider added", |s| {
        s["providers"]["extra"].is_string()
    });
    assert!(fs::symlink_metadata(&file).unwrap().file_type().is_file());
    assert_eq!(
        fs::read_to_string(&decoy).unwrap(),
        r#"{"planted":"https://planted.example"}"#
    );
}

#[test]
fn s06_permissive_data_directories_are_tightened_and_reported() {
    let mut sb = rust_daemon!("s06");
    let chat = FakeChat::start();
    let dirs = sb.data_dirs();
    for d in &dirs {
        fs::create_dir_all(d).unwrap();
        fs::set_permissions(d, fs::Permissions::from_mode(0o755)).unwrap();
    }
    sb.start_hidden(&chat.url());
    for d in &dirs {
        assert_eq!(mode(d), 0o700, "{}", d.display());
    }
    let log = sb.daemon_log(0);
    assert!(
        log.contains("0755") && log.contains("0700"),
        "not reported: {log}"
    );
}

#[test]
fn s07_a_refused_first_command_leaves_nothing_behind() {
    let sb = rust_daemon!("s07");
    let out = sb.run(&["--provider", "no-such-provider"]);
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("no-such-provider"), "{}", out.stderr);
    thread::sleep(Duration::from_millis(300));
    assert!(sb.daemon_pids().is_empty(), "a daemon was left running");
    assert!(!sb.socket_path(None).exists() && !sb.state_path(None).exists());
}

#[test]
fn s08_sigterm_is_a_clean_shutdown() {
    let mut sb = rust_daemon!("s08");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    let pid = sb.daemon_pids()[0];
    assert!(Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    assert!(
        sb.wait_gone(None, SECS(8)),
        "socket/state still there after SIGTERM"
    );
    let deadline = Instant::now() + SECS(8);
    while !sb.daemon_exited(0) {
        assert!(Instant::now() < deadline, "the daemon did not exit");
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(sb.daemon_exit_code(0), Some(0));
}

#[test]
fn s09_a_killed_daemon_does_not_block_the_next_one() {
    let mut sb = rust_daemon!("s09");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    let pid = sb.daemon_pids()[0];
    proc::kill9(pid);
    // The kernel closes a dead process's sockets a moment after the process is gone; until then a
    // connection is accepted into the dying listener and reset. Wait for the plain "refused".
    let deadline = Instant::now() + SECS(5);
    loop {
        match UnixStream::connect(sb.socket_path(None)) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => break,
            _ => {
                assert!(
                    Instant::now() < deadline,
                    "the dead daemon's socket never closed"
                );
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
    // a stale socket, state file and lock file are all that is left; the start must not be refused
    assert!(sb.socket_path(None).exists());
    assert_eq!(sb.state(None).unwrap()["visible"], false);
    sb.start(&["--show"]);
    sb.wait_state(None, "a new daemon to show its window", |s| {
        s["visible"] == true
    });
    assert_eq!(sb.daemon_pids().len(), 1);
}

#[test]
fn s10_a_normal_session_raises_no_toolkit_warnings() {
    let mut sb = rust_daemon!("s10");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    for args in [
        &["--show"][..],
        &["--new-chat"],
        &["--soft-clean"],
        &["--hard-clean"],
        &["--idle-timeout", "20"],
        &["--user-agent", "E2E-Test/1.0"],
        &["--add-provider", "extra", "https://extra.example"],
        &["--remove-provider", "extra"],
        &["--hide"],
        &["--toggle"],
        &["--toggle"],
    ] {
        let out = sb.run(args);
        assert_eq!(out.code, 0, "{args:?}: {}", out.stderr);
    }
    sb.quit(None);
    assert!(sb.wait_gone(None, SECS(8)));
    // GLib/GTK/WebKit report programming errors (a wrong argument, a bad call order) as
    // CRITICAL/WARNING lines on stderr and carry on; none may appear. Two notes are environmental: the
    // EGL ones (the virtual X server used by the tests has no DRI3) and the missing accessibility bus.
    let bad: Vec<String> = sb
        .daemon_log(0)
        .lines()
        .filter(|l| !l.starts_with("libEGL"))
        // the sandboxed web process looks for the accessibility bus, which the test session lacks
        .filter(|l| !l.contains("Can't connect to a11y bus"))
        .filter(|l| l.contains("CRITICAL") || l.contains("WARNING") || l.contains("-Warning"))
        .map(str::to_owned)
        .collect();
    assert!(bad.is_empty(), "toolkit warnings:\n  {}", bad.join("\n  "));
}

#[test]
fn s11_a_second_daemon_binary_hands_its_command_over_and_reports_the_verdict() {
    // The client normally finds the running daemon itself; this is the race it cannot see: two
    // `llm-wrapperd` started at once, the loser forwards its own command line.
    let mut sb = rust_daemon!("s11");
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    let pid = sb.daemon_pids()[0];

    let ok = sb.run_daemon_binary(&["--show"]);
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    sb.wait_state(None, "window visible", |s| s["visible"] == true);

    let refused = sb.run_daemon_binary(&["--provider", "no-such-provider"]);
    assert_eq!(
        refused.code, 1,
        "a refused command must fail: {}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("no-such-provider"),
        "{}",
        refused.stderr
    );

    let usage = sb.run_daemon_binary(&["--toogle"]);
    assert_eq!(usage.code, 2, "{}", usage.stderr);

    assert_eq!(sb.daemon_pids(), vec![pid], "a second daemon was started");
    assert!(sb.visible(None), "the refused command changed something");
}

#[test]
fn s12_switching_provider_with_new_chat_loads_the_page_once() {
    let mut sb = rust_daemon!("s12");
    let first = FakeChat::start();
    let second = FakeChat::start();
    sb.start_hidden(&first.url());
    let url = second.url();
    assert_eq!(sb.run(&["--provider", &url, "--new-chat"]).code, 0);
    assert!(
        second.wait(SECS(10), |h| !h.is_empty()),
        "the new provider was never loaded"
    );
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        second.count("/"),
        1,
        "the provider's page was requested {} times",
        second.count("/")
    );
}

// ---- Phase 3: what the pages may do ---------------------------------------------------------------

/// Start a hidden daemon whose provider page is `path` on a fresh fake chat site.
fn on_page(sb: &mut Sandbox, chat: &FakeChat, path: &str) {
    let url = format!("{}{path}", chat.url());
    sb.start_hidden(&url);
}

#[test]
fn s13_every_permission_a_page_asks_for_is_denied() {
    let mut sb = rust_daemon!("s13");
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "perm");
    assert!(
        chat.wait(SECS(30), |h| h
            .iter()
            .any(|h| h.path.starts_with("/report-perm?"))),
        "the page never got an answer; it reached {:?}; daemon said {:?}",
        chat.hits().iter().map(|h| h.query()).collect::<Vec<_>>(),
        sb.daemon_log(0)
    );
    let answers = chat
        .hits()
        .iter()
        .find(|h| h.path.starts_with("/report-perm?"))
        .unwrap()
        .json();
    assert_eq!(answers["notification"], "denied", "{answers}");
    // geolocation: this WebKit build may never even ask; it must not say yes
    assert_ne!(answers["geolocation"], "granted", "{answers}");
    assert!(
        answers["clipboard"]
            .as_str()
            .unwrap()
            .starts_with("denied:")
            || answers["clipboard"] == "no clipboard api",
        "{answers}"
    );
    // media capture is switched off altogether (docs/spec.md V11): there is nothing to ask for
    assert_ne!(answers["getUserMedia"], "function", "{answers}");
    // (a request made without a click never reaches the app; k06 covers the one that does)
}

#[test]
fn s14_a_sign_in_popup_opens_in_a_window_of_its_own_with_the_same_environment() {
    let mut sb = rust_daemon!("s14");
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "open?u=%2Fprobe");
    assert!(
        chat.wait(SECS(30), |h| h
            .iter()
            .any(|h| h.path.starts_with("/report?"))),
        "the popup never loaded: {:?}",
        chat.hits()
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>()
    );
    let report = chat
        .hits()
        .iter()
        .find(|h| h.path.starts_with("/report?"))
        .unwrap()
        .json();
    // the popup shares the session settings and the compatibility script
    assert_eq!(report["webdriver"], false, "{report}");
    assert_eq!(report["vendor"], "Google Inc.", "{report}");
    assert_eq!(report["contain"], "layout style", "{report}");
    assert!(sb.browser_opened().is_empty());
}

#[test]
fn s15_a_page_cannot_open_more_than_four_windows() {
    let mut sb = rust_daemon!("s15");
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "open-many");
    assert!(chat.wait(SECS(30), |h| h.iter().any(|h| h.path == "/opener-ran")));
    thread::sleep(Duration::from_secs(3));
    let opened: std::collections::BTreeSet<String> = chat
        .hits()
        .iter()
        .filter(|h| h.path.starts_with("/popup-page?"))
        .map(|h| h.path.clone())
        .collect();
    assert_eq!(opened.len(), 4, "windows that loaded: {opened:?}");
    assert!(
        sb.daemon_log(0).contains("too many pop-ups"),
        "{}",
        sb.daemon_log(0)
    );
}

#[test]
fn s16_a_page_cannot_send_the_user_to_the_browser_without_a_click() {
    let mut sb = rust_daemon!("s16");
    sb.install_fake_browser();
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "open?u=http%3A%2F%2Fexternal.test%2Fx");
    assert!(chat.wait(SECS(30), |h| h.iter().any(|h| h.path == "/opener-ran")));
    thread::sleep(Duration::from_secs(2));
    assert!(
        sb.browser_opened().is_empty(),
        "opened: {:?}",
        sb.browser_opened()
    );
    assert!(
        sb.daemon_log(0)
            .contains("refused a window for a http address"),
        "{}",
        sb.daemon_log(0)
    );
}

#[test]
fn s17_a_page_asking_for_a_hundred_downloads_gets_at_most_one_dialog() {
    // Without a person to answer the dialog, the first download waits on it and every other one
    // must be refused at once rather than piling up windows.
    let mut sb = rust_daemon!("s17");
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "download-many");
    assert!(chat.wait(SECS(30), |h| h
        .iter()
        .filter(|h| h.path.starts_with("/download?"))
        .count()
        >= 2));
    thread::sleep(Duration::from_secs(2));
    let log = sb.daemon_log(0);
    assert!(log.contains("another file dialog is already open"), "{log}");
    let dialogs = proc::windows_named(sb.daemon_pids()[0], "Save File").len();
    assert_eq!(dialogs, 1, "{dialogs} save dialogs are open");
}

#[test]
fn s18_closed_popups_make_room_for_new_ones() {
    let mut sb = rust_daemon!("s18");
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "open-cycle");
    // four pop-ups come and close themselves; the four that follow must all be let through
    assert!(
        chat.wait(SECS(30), |h| h
            .iter()
            .filter(|h| h.path.starts_with("/popup-page?n=late"))
            .count()
            == 4),
        "pop-ups that loaded: {:?}",
        chat.hits()
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>()
    );
}
