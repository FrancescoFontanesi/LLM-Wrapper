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
    // the refusal is logged as the further downloads reach the daemon: wait for it, not for a
    // fixed time (a loaded machine took more than two seconds once)
    assert!(
        until(SECS(15), || sb
            .daemon_log(0)
            .contains("another file dialog is already open")),
        "{}",
        sb.daemon_log(0)
    );
    // The dialog's window may carry no process id (it does not when the display is busy), so it is
    // found by its title; no other test in this binary opens a save dialog.
    assert!(
        until(SECS(15), || !proc::windows_titled("Save File").is_empty()),
        "no save dialog appeared"
    );
    thread::sleep(SECS(1));
    let dialogs = proc::windows_titled("Save File").len();
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

// ---- Phase 4: a hidden, idle page goes back to the provider's home page ---------------------------

/// `--idle-timeout 1` is the shortest the command line allows, so these take over a minute.
macro_rules! slow {
    () => {
        if std::env::var_os("LW_SLOW").is_none() {
            skip_by_design!("slow (about 70 s): set LW_SLOW=1");
        }
    };
}

const WENT_HOME: &str = "back to the provider's home page";

fn web_pids(pid: u32) -> Vec<u32> {
    proc::descendants(pid)
        .into_iter()
        .filter(|p| proc::cmdline(*p).contains("WebKitWebProcess"))
        .collect()
}

fn web_processes(pid: u32) -> usize {
    web_pids(pid).len()
}

/// Wait for `cond` for up to `limit`.
fn until(limit: Duration, cond: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    cond()
}

/// How many times the fake site was asked for its home page (the `/jump` address) in `hits`.
/// (Inside `FakeChat::wait` use the hits it hands over: asking the site again would lock twice.)
fn jumps(hits: &[llm_wrapper_e2e::fakechat::Hit]) -> usize {
    hits.iter().filter(|h| h.path.starts_with("/jump")).count()
}

fn home_loads(chat: &FakeChat) -> usize {
    jumps(&chat.hits())
}

fn paths(chat: &FakeChat) -> Vec<String> {
    chat.hits().iter().map(|h| h.path.clone()).collect()
}

/// A provider address that sends you on to `to`: "where you were" is not the provider's own page.
fn jump_to(chat: &FakeChat, to: &str) -> String {
    let encoded: String = to
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("{}jump?to={encoded}", chat.url())
}

#[test]
fn s19_a_hidden_idle_page_goes_back_to_the_home_page_and_is_there_when_shown() {
    slow!();
    let mut sb = rust_daemon!("s19");
    let chat = FakeChat::start();
    let url = jump_to(&chat, "/c/abc-123");
    let pid = sb.start(&["--provider", &url]);
    sb.wait_state(None, "daemon up", |s| s["url"] == url.as_str());
    assert!(chat.wait(SECS(20), |h| h.iter().any(|h| h.path == "/c/abc-123")));
    assert_eq!(home_loads(&chat), 1);
    let before_pids = web_pids(pid);
    assert!(!before_pids.is_empty(), "no web process to begin with");
    assert_eq!(sb.run(&["--idle-timeout", "1"]).code, 0);
    assert_eq!(sb.run(&["--hide"]).code, 0);

    assert!(
        until(SECS(100), || sb.daemon_log(0).contains(WENT_HOME)),
        "the page never went back home; daemon: {}",
        sb.daemon_log(0)
    );
    assert!(
        chat.wait(SECS(15), |h| jumps(h) == 2),
        "the home page was not loaded: {:?}",
        paths(&chat)
    );
    // the page the conversation lived in is gone with its process (that is what gives the memory
    // back), and the home page is in a new one, ready: showing the window has nothing left to load
    assert!(
        until(SECS(15), || {
            let now = web_pids(pid);
            !now.is_empty() && now.iter().all(|p| !before_pids.contains(p))
        }),
        "the old web process is still there, or there is no new one: {:?} then {:?}",
        before_pids,
        web_pids(pid)
    );
    let before = chat.total();
    assert_eq!(sb.state(None).unwrap()["visible"], false);
    assert_eq!(sb.state(None).unwrap()["url"], url.as_str());
    assert_eq!(sb.run(&["--hide"]).code, 0);
    assert_eq!(sb.run(&["--show"]).code, 0);
    sb.wait_state(None, "window visible", |s| s["visible"] == true);
    thread::sleep(SECS(2));
    assert_eq!(
        chat.total(),
        before,
        "showing the window loaded something: {:?}",
        paths(&chat)
    );
    assert_eq!(home_loads(&chat), 2, "{:?}", paths(&chat));
}

#[test]
fn s20_the_draft_probe_sees_every_kind_of_unsent_input() {
    let mut sb = rust_daemon!("s20");
    let chat = FakeChat::start();
    on_page(&mut sb, &chat, "draft-matrix");
    assert!(
        chat.wait(SECS(30), |h| h
            .iter()
            .any(|h| h.path.starts_with("/report-draft?"))),
        "the page never reported; daemon: {}",
        sb.daemon_log(0)
    );
    let got = chat
        .hits()
        .iter()
        .find(|h| h.path.starts_with("/report-draft?"))
        .unwrap()
        .json();
    let expect = [
        ("empty", false),
        ("textarea", true),
        ("textarea-blank", false),
        ("text-input", true),
        ("input-without-type", true),
        ("search", true),
        ("password", true),
        ("not-text", false),
        ("contenteditable", true),
        ("contenteditable-empty", false),
        ("contenteditable-newline", false),
        ("contenteditable-false", false),
        ("contenteditable-plaintext", true),
        ("file", true),
        ("file-empty", false),
        // anything the probe cannot read keeps the page
        ("unreadable", true),
    ];
    for (scene, want) in expect {
        assert_eq!(got[scene], want, "{scene}: {got}");
    }
    assert_eq!(got.as_object().unwrap().len(), expect.len(), "{got}");
}

#[test]
fn s21_a_page_with_an_unsent_message_is_kept_until_it_is_sent() {
    slow!();
    let mut sb = rust_daemon!("s21");
    let chat = FakeChat::start();
    // the conversation page is "sent" (the box emptied) 75 s after it loads: after the first look
    // at 60 s, before the second at 120 s
    let url = jump_to(&chat, "/draft?clear=75000");
    sb.start(&["--provider", &url]);
    sb.wait_state(None, "daemon up", |s| s["url"] == url.as_str());
    assert!(chat.wait(SECS(20), |h| h.iter().any(|h| h.path.starts_with("/draft"))));
    assert_eq!(sb.run(&["--idle-timeout", "1"]).code, 0);
    assert_eq!(sb.run(&["--hide"]).code, 0);
    let hits = chat.total();

    // first look: kept
    assert!(
        until(SECS(80), || sb
            .daemon_log(0)
            .contains("holds something unsent")),
        "the draft was not noticed; daemon: {}",
        sb.daemon_log(0)
    );
    assert!(!sb.daemon_log(0).contains(WENT_HOME));
    assert_eq!(chat.total(), hits, "the page was reloaded");

    // sent: home at the next look
    assert!(chat.wait(SECS(60), |h| h.iter().any(|h| h.path == "/draft-sent")));
    assert!(
        until(SECS(100), || sb.daemon_log(0).contains(WENT_HOME)),
        "the page never went home once the message was sent; daemon: {}",
        sb.daemon_log(0)
    );
    // the log line comes as the load is started: the request follows once the new process is up
    assert!(
        chat.wait(SECS(15), |h| jumps(h) == 2),
        "the home page was not loaded: {:?}",
        paths(&chat)
    );
}

#[test]
fn s22_a_daemon_started_in_the_background_goes_home_too_and_switching_providers_still_works() {
    slow!();
    let mut sb = rust_daemon!("s22");
    let (first, second) = (FakeChat::start(), FakeChat::start());
    let url = jump_to(&first, "/c/abc-123");
    let pid = sb.start(&["--background", "--idle-timeout", "1", "--provider", &url]);
    sb.wait_state(None, "daemon up", |s| s["url"] == url.as_str());
    assert!(first.wait(SECS(20), |h| h.iter().any(|h| h.path == "/c/abc-123")));
    // never shown, never hidden: the idle time counts from the start
    assert!(
        until(SECS(100), || sb.daemon_log(0).contains(WENT_HOME)),
        "a daemon started hidden never went home; daemon: {}",
        sb.daemon_log(0)
    );
    assert!(first.wait(SECS(15), |h| jumps(h) == 2));
    assert!(web_processes(pid) >= 1);
    let (before_first, before_second) = (first.total(), second.total());
    // cleaning loads nothing
    assert_eq!(sb.run(&["--soft-clean"]).code, 0);
    assert_eq!(sb.run(&["--hard-clean"]).code, 0);
    thread::sleep(SECS(1));
    assert_eq!(first.total(), before_first, "a clean-up loaded the page");

    // a provider chosen in the background is loaded at once, without a window
    let third = FakeChat::start();
    let background = third.url();
    assert_eq!(sb.run(&["--background", "--provider", &background]).code, 0);
    sb.wait_state(None, "provider switched", |s| {
        s["url"] == background.as_str()
    });
    assert!(
        third.wait(SECS(15), |h| !h.is_empty()),
        "a provider chosen in the background was not loaded"
    );
    assert!(!sb.visible(None));

    // another provider: that page loads, the old one is not asked for again
    let other = second.url();
    assert_eq!(sb.run(&["--provider", &other]).code, 0);
    sb.wait_state(None, "provider switched", |s| s["url"] == other.as_str());
    assert!(second.wait(SECS(15), |h| h.len() > before_second));
    assert_eq!(first.total(), before_first);
    assert!(sb.visible(None));
}

#[test]
fn s23_a_new_chat_after_going_home_still_works_and_an_open_popup_keeps_the_page() {
    slow!();
    let mut sb = rust_daemon!("s23");
    // two daemons (instances): one goes home and is asked for a new chat, one has a pop-up open
    let chat = FakeChat::start();
    let url = jump_to(&chat, "/c/abc-123");
    sb.start(&[
        "--instance",
        "a",
        "--background",
        "--idle-timeout",
        "1",
        "--provider",
        &url,
    ]);
    sb.wait_state(Some("a"), "instance a up", |s| s["url"] == url.as_str());
    let busy_chat = FakeChat::start();
    let busy_url = jump_to(&busy_chat, "/open?u=%2Fpopup-page");
    sb.start(&[
        "--instance",
        "b",
        "--background",
        "--idle-timeout",
        "1",
        "--provider",
        &busy_url,
    ]);
    sb.wait_state(Some("b"), "instance b up", |s| {
        s["url"] == busy_url.as_str()
    });
    assert!(busy_chat.wait(SECS(20), |h| h
        .iter()
        .any(|h| h.path.starts_with("/popup-page"))));

    assert!(
        until(SECS(100), || sb.daemon_log(0).contains(WENT_HOME)),
        "instance a never went home; daemon: {}",
        sb.daemon_log(0)
    );
    assert!(chat.wait(SECS(15), |h| jumps(h) == 2));
    let before = home_loads(&chat);
    assert_eq!(sb.run(&["--instance", "a", "--new-chat"]).code, 0);
    assert!(
        chat.wait(SECS(15), |h| jumps(h) > before),
        "the provider's own address was not loaded: {:?}",
        paths(&chat)
    );

    // the one with a pop-up open stayed where it was, however long it has been hidden
    assert!(
        !sb.daemon_log(1).contains(WENT_HOME),
        "{}",
        sb.daemon_log(1)
    );
    assert_eq!(home_loads(&busy_chat), 1, "{:?}", paths(&busy_chat));
}

#[test]
fn s24_a_page_that_is_loading_has_a_dialog_open_or_hides_its_draft_does_not_go_home() {
    slow!();
    let mut sb = rust_daemon!("s24");
    let quiet = FakeChat::start(); // the control: nothing to keep, so it goes home
    let (loading, dialog, hostile) = (FakeChat::start(), FakeChat::start(), FakeChat::start());
    let late = FakeChat::start(); // busy at the first look, quiet by the second
    let chats = [&quiet, &loading, &dialog, &hostile, &late];
    let urls = [
        jump_to(&quiet, "/c/abc-123"),
        jump_to(&loading, "/slow-load"),
        jump_to(&dialog, "/download-many"),
        jump_to(&hostile, "/draft-hostile"),
        jump_to(&late, "/slow-load?finish=75000"),
    ];
    for (i, url) in urls.iter().enumerate() {
        let name = format!("i{i}");
        sb.start(&[
            "--instance",
            &name,
            "--background",
            "--idle-timeout",
            "1",
            "--provider",
            url,
        ]);
        sb.wait_state(Some(&name), "instance up", |s| s["url"] == url.as_str());
    }
    assert!(dialog.wait(SECS(30), |h| h
        .iter()
        .any(|h| h.path.starts_with("/download?"))));

    // the control goes; the others are looked at at least once more (a busy page every minute)
    assert!(
        until(SECS(100), || sb.daemon_log(0).contains(WENT_HOME)),
        "the control never went home; daemon: {}",
        sb.daemon_log(0)
    );
    thread::sleep(SECS(45));
    for (i, what) in [
        (1, "a page still loading"),
        (2, "a page with a dialog open"),
        (3, "a page hiding its draft"),
    ] {
        assert!(!sb.daemon_log(i).contains(WENT_HOME), "{what} went home");
        assert_eq!(
            home_loads(chats[i]),
            1,
            "{what} was reloaded: {:?}",
            paths(chats[i])
        );
    }
    assert!(
        sb.daemon_log(3).contains("holds something unsent"),
        "the hostile page hid its draft: {}",
        sb.daemon_log(3)
    );
    // the one that was busy only for a while goes home at a later look
    assert!(
        until(SECS(100), || sb.daemon_log(4).contains(WENT_HOME)),
        "a page that finished loading never went home; daemon: {}",
        sb.daemon_log(4)
    );
}
