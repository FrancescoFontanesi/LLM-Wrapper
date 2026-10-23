//! Behaviour that must be identical in the legacy app and the Rust implementation ("P" tests).
//! Legacy behaviours that are kept on purpose are marked `quirk` and listed in `docs/spec.md`.

use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use llm_wrapper_e2e::proc;
use llm_wrapper_e2e::{skip, FakeChat, Sandbox};
use serde_json::{json, Value};

const SECS: fn(u64) -> Duration = Duration::from_secs;

fn builtins() -> Value {
    json!({"chatgpt":"https://chatgpt.com","gemini":"https://gemini.google.com","claude":"https://claude.ai"})
}

/// Skip the test (not the suite) while the Rust daemon does not exist.
macro_rules! daemon_or_skip {
    ($sb:expr) => {
        if !$sb.has_daemon() {
            skip!("no Rust daemon built, or no display to open its window");
        }
    };
}

fn running(label: &str) -> Option<(Sandbox, FakeChat)> {
    let mut sb = Sandbox::new_or_skip(label)?;
    if !sb.has_daemon() {
        return None;
    }
    let chat = FakeChat::start();
    sb.start_hidden(&chat.url());
    Some((sb, chat))
}

fn expect_ok(sb: &Sandbox, args: &[&str]) {
    let out = sb.run(args);
    assert_eq!(out.code, 0, "{args:?} failed: {}{}", out.stdout, out.stderr);
}

fn wait_visible(sb: &Sandbox, inst: Option<&str>, want: bool) {
    sb.wait_state(
        inst,
        if want {
            "window visible"
        } else {
            "window hidden"
        },
        |s| s["visible"] == want,
    );
}

fn names(state: &Value) -> Vec<String> {
    let mut v: Vec<String> = state["providers"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    v.sort();
    v
}

// ---- state and visibility ---------------------------------------------------------------------

#[test]
fn p01_fresh_start_reports_the_documented_state() {
    let Some(mut sb) = Sandbox::new_or_skip("p01") else {
        return;
    };
    daemon_or_skip!(sb);
    sb.start(&["--background"]);
    let s = sb.wait_state(None, "initial state", |s| s["active"] == "chatgpt");
    let mut keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["active", "instance", "providers", "url", "visible"],
        "the state file schema is a public API (Quickshell reads it)"
    );
    assert_eq!(s["instance"], json!(null));
    assert_eq!(s["url"], "https://chatgpt.com");
    assert_eq!(s["visible"], false, "--background must not show the window");
    assert_eq!(s["providers"], builtins());
}

#[test]
fn p02_visibility_commands_and_their_precedence() {
    let Some((sb, _chat)) = running("p02") else {
        skip!("no Rust daemon yet")
    };
    assert!(!sb.visible(None));

    expect_ok(&sb, &["--show"]);
    wait_visible(&sb, None, true);
    expect_ok(&sb, &["--hide"]);
    wait_visible(&sb, None, false);
    expect_ok(&sb, &["--toggle"]);
    wait_visible(&sb, None, true);
    expect_ok(&sb, &["--toggle"]);
    wait_visible(&sb, None, false);

    // `--new-chat` is bound to a key to start a chat you are about to read: it shows the window.
    // (`--provider` does too, see p03. The cleaning and settings commands do not: see D13.)
    expect_ok(&sb, &["--new-chat"]);
    wait_visible(&sb, None, true);
    expect_ok(&sb, &["--hide"]);
    wait_visible(&sb, None, false);

    // precedence: --toggle beats --show; --show beats --hide; --background hides
    expect_ok(&sb, &["--show"]);
    wait_visible(&sb, None, true);
    expect_ok(&sb, &["--toggle", "--show"]);
    wait_visible(&sb, None, false);
    expect_ok(&sb, &["--hide", "--show"]);
    wait_visible(&sb, None, true);
    expect_ok(&sb, &["--background"]);
    wait_visible(&sb, None, false);
}

#[test]
fn p03_switching_providers_by_name_and_by_url() {
    let Some((sb, chat1)) = running("p03") else {
        skip!("no Rust daemon yet")
    };
    let chat2 = FakeChat::start();
    expect_ok(&sb, &["--add-provider", "other", &chat2.url()]);
    sb.wait_state(None, "provider added", |s| {
        s["providers"]["other"] == chat2.url().as_str()
    });

    expect_ok(&sb, &["--provider", "other"]);
    wait_visible(&sb, None, true); // README: `--provider` "shows the window"
    let s = sb.wait_state(None, "switch to named provider", |s| s["active"] == "other");
    assert_eq!(s["url"], chat2.url().as_str());
    assert!(
        chat2.wait(SECS(15), |h| h.iter().any(|x| x.path == "/")),
        "the new provider was never loaded"
    );

    expect_ok(&sb, &["--provider", "gemini"]);
    let s = sb.wait_state(None, "switch to built-in", |s| s["active"] == "gemini");
    assert_eq!(s["url"], "https://gemini.google.com");

    // an ad-hoc URL is both the active name and the url
    expect_ok(&sb, &["--provider", &chat1.url()]);
    let s = sb.wait_state(None, "switch to ad-hoc url", |s| {
        s["active"] == chat1.url().as_str()
    });
    assert_eq!(s["url"], chat1.url().as_str());
}

#[test]
fn p04_providers_are_edited_and_persisted() {
    let Some((mut sb, chat)) = running("p04") else {
        skip!("no Rust daemon yet")
    };
    expect_ok(
        &sb,
        &["--add-provider", "mistral", "https://chat.mistral.ai"],
    );
    expect_ok(&sb, &["--remove-provider", "claude"]);
    let s = sb.wait_state(None, "edited list", |s| {
        s["providers"]["mistral"] == "https://chat.mistral.ai"
            && s["providers"].get("claude").is_none()
    });
    assert_eq!(names(&s), ["chatgpt", "gemini", "mistral"]);

    // invalid additions are ignored (whether the CLI or the daemon says so is the B-tests' business)
    let _ = sb.run(&["--add-provider", "bad name", "https://x.example"]);
    let _ = sb.run(&["--add-provider", "ok", "ftp://x.example"]);
    let _ = sb.run(&["--remove-provider", "no-such-provider"]);
    thread::sleep(Duration::from_millis(400));
    assert_eq!(
        names(&sb.state(None).unwrap()),
        ["chatgpt", "gemini", "mistral"]
    );

    sb.quit(None);
    assert!(sb.wait_gone(None, SECS(10)));
    sb.start_hidden(&chat.url());
    let s = sb.wait_state(None, "reloaded list", |s| s["url"] == chat.url().as_str());
    assert_eq!(
        names(&s),
        ["chatgpt", "gemini", "mistral"],
        "the provider list must survive a restart"
    );
}

#[test]
fn p05_runtime_dir_holds_a_socket_and_a_whole_state_file() {
    let Some((sb, _chat)) = running("p05") else {
        skip!("no Rust daemon yet")
    };
    for flag in ["--show", "--hide", "--toggle", "--hide"] {
        expect_ok(&sb, &[flag]);
    }
    assert!(sb.socket_path(None).exists());
    // The daemon answers before it has finished writing, so the transient `*.tmp` of the atomic
    // rewrite may still be there for a moment. The invariant is that it never stays.
    let start = Instant::now();
    loop {
        let leftovers: Vec<_> = std::fs::read_dir(&sb.runtime)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        if leftovers.is_empty() {
            break;
        }
        assert!(
            start.elapsed() < SECS(3),
            "temporary files left behind: {leftovers:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

// ---- navigation --------------------------------------------------------------------------------

#[test]
fn p06_new_chat_loads_the_provider_root_again() {
    let Some((sb, chat)) = running("p06") else {
        skip!("no Rust daemon yet")
    };
    assert!(
        chat.wait(SECS(15), |h| !h.is_empty()),
        "the provider was never loaded"
    );
    let before = chat.count("/");
    expect_ok(&sb, &["--new-chat"]);
    assert!(
        chat.wait(SECS(10), |h| h.iter().filter(|x| x.path == "/").count()
            > before),
        "--new-chat did not reload the root"
    );
}

#[test]
fn p07_user_agent_override_reaches_the_server() {
    let Some((sb, chat)) = running("p07") else {
        skip!("no Rust daemon yet")
    };
    expect_ok(&sb, &["--user-agent", "E2E-Test/1.2 (override)"]);
    expect_ok(&sb, &["--new-chat"]);
    assert!(
        chat.wait(SECS(10), |h| h
            .last()
            .is_some_and(|x| x.user_agent == "E2E-Test/1.2 (override)")),
        "{:?}",
        chat.last()
    );
}

#[test]
fn p07b_default_user_agent_is_chrome_on_linux() {
    let Some((sb, chat)) = running("p07b") else {
        skip!("no Rust daemon yet")
    };
    assert!(chat.wait(SECS(15), |h| !h.is_empty()));
    let ua = &chat.hits()[0].user_agent;
    let prefix = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/";
    assert!(ua.starts_with(prefix), "{ua}");
    let version = ua[prefix.len()..].strip_suffix(".0.0.0 Safari/537.36");
    let major: u32 = version
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("not a reduced Chrome user agent: {ua}"));
    if sb.is_legacy() {
        assert_eq!(major, 131, "the legacy app's fixed value");
    } else {
        // deliberately newer than the legacy app's (docs/spec.md V15): sites turn away old Chrome
        assert!(major >= 131, "{ua}");
    }
}

#[test]
fn p16_pages_see_a_chrome_environment_in_every_frame() {
    let Some(mut sb) = Sandbox::new_or_skip("p16") else {
        return;
    };
    daemon_or_skip!(sb);
    let chat = FakeChat::start();
    let url = format!("{}probe", chat.url());
    sb.start_hidden(&url);
    assert!(
        chat.wait(SECS(20), |h| h
            .iter()
            .any(|h| h.path.starts_with("/report?"))
            && h.iter().any(|h| h.path.starts_with("/report-frame?"))),
        "the probe page never reported: {:?}",
        chat.hits()
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>()
    );
    let hits = chat.hits();
    let page = hits
        .iter()
        .find(|h| h.path.starts_with("/report?"))
        .unwrap()
        .json();
    let frame = hits
        .iter()
        .find(|h| h.path.starts_with("/report-frame?"))
        .unwrap()
        .json();

    assert_eq!(page["webdriver"], false);
    assert_eq!(page["vendor"], "Google Inc.");
    assert_eq!(page["chrome"], "object");
    assert_eq!(page["runtime"], true);
    // WebKit lists its own PDF viewers (five); the app only fills in an empty list with three
    assert!(page["plugins"].as_u64().unwrap() >= 3, "{page}");
    assert_eq!(page["mobile"], false);
    assert_eq!(page["platform"], "Linux");
    assert_eq!(page["arch"], "x86");
    assert_eq!(page["bitness"], "64");
    // the brands name the Chrome the user agent names
    let ua = page["ua"].as_str().unwrap();
    let major = ua
        .split("Chrome/")
        .nth(1)
        .and_then(|r| r.split('.').next())
        .unwrap();
    let brands = page["brands"].as_str().unwrap();
    assert_eq!(
        brands,
        format!("Chromium/{major},Not_A Brand/24,Google Chrome/{major}")
    );
    let full = page["full"].as_str().unwrap();
    assert!(
        full.split(',')
            .next()
            .unwrap()
            .starts_with(&format!("{major}.")),
        "{full}"
    );
    // the app's style sheet is in the page
    assert_eq!(page["contain"], "layout style");
    // and in frames of their own, where the bot checks run
    assert_eq!(frame["webdriver"], false);
    assert_eq!(frame["vendor"], "Google Inc.");
    assert_eq!(frame["brands"], 3);
}

// ---- instances, window class, lifecycle ---------------------------------------------------------

#[test]
fn p08_instances_are_independent() {
    let Some(mut sb) = Sandbox::new_or_skip("p08") else {
        return;
    };
    daemon_or_skip!(sb);
    let (a, b) = (FakeChat::start(), FakeChat::start());
    let pid_a = sb.start(&["--instance", "a", "--background", "--provider", &a.url()]);
    sb.start(&["--instance", "b", "--background", "--provider", &b.url()]);
    let sa = sb.wait_state(Some("a"), "instance a", |s| s["url"] == a.url().as_str());
    let sbb = sb.wait_state(Some("b"), "instance b", |s| s["url"] == b.url().as_str());
    assert_eq!(
        (sa["instance"].as_str(), sbb["instance"].as_str()),
        (Some("a"), Some("b"))
    );

    expect_ok(&sb, &["--instance", "a", "--show"]);
    wait_visible(&sb, Some("a"), true);
    assert!(
        !sb.visible(Some("b")),
        "showing instance a must not show instance b"
    );

    if proc::have_x11_tools() {
        let start = Instant::now();
        let mut class = None;
        while start.elapsed() < SECS(8) && class.is_none() {
            class = proc::wm_class(pid_a);
            thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            class,
            Some(("llm-wrapper-a".into(), "llm-wrapper-a".into())),
            "Hyprland rules match on this class"
        );
    }

    expect_ok(&sb, &["--instance", "a", "--quit"]);
    assert!(sb.wait_gone(Some("a"), SECS(10)));
    expect_ok(&sb, &["--instance", "b", "--show"]);
    wait_visible(&sb, Some("b"), true);
}

#[test]
fn p09_window_class_is_llm_wrapper() {
    if !proc::have_x11_tools() {
        skip!("xdotool/xprop or an X display are not available");
    }
    let Some(mut sb) = Sandbox::new_or_skip("p09") else {
        return;
    };
    daemon_or_skip!(sb);
    let chat = FakeChat::start();
    let pid = sb.start(&["--provider", &chat.url()]);
    sb.wait_state(None, "daemon up", |s| s["visible"] == true);
    let start = Instant::now();
    let mut class = None;
    while start.elapsed() < SECS(10) && class.is_none() {
        class = proc::wm_class(pid);
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(class, Some(("llm-wrapper".into(), "llm-wrapper".into())));
}

#[test]
fn p10_quit_removes_everything_and_ignores_other_flags() {
    let Some((mut sb, _chat)) = running("p10") else {
        skip!("no Rust daemon yet")
    };
    let pids = proc::pids_in(sb.runtime.to_str().unwrap());
    assert!(
        pids.len() >= 3,
        "expected the daemon plus WebKit helpers, saw {pids:?}"
    );
    // quirk: --quit wins over everything else on the line
    expect_ok(&sb, &["--quit", "--provider", "gemini", "--show"]);
    assert!(
        sb.wait_gone(None, SECS(10)),
        "socket/state file still present"
    );
    let start = Instant::now();
    loop {
        let left: Vec<_> = proc::pids_in(sb.runtime.to_str().unwrap())
            .into_iter()
            .filter(|p| proc::alive(*p) && !proc::cmdline(*p).contains("dbus-daemon"))
            .collect();
        if left.is_empty() {
            break;
        }
        assert!(
            start.elapsed() < SECS(10),
            "processes survived --quit: {:?}",
            left.iter().map(|p| proc::cmdline(*p)).collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(50));
    }
    let _ = &mut sb;
}

#[test]
fn p11_repeated_commands_never_start_a_second_daemon() {
    let Some((sb, _chat)) = running("p11") else {
        skip!("no Rust daemon yet")
    };
    for flag in [
        "--background",
        "--background",
        "--show",
        "--hide",
        "--background",
    ] {
        expect_ok(&sb, &[flag]);
    }
    assert_eq!(sb.daemon_pids().len(), 1, "{:?}", sb.daemon_pids());
}

fn wait_socket_accepts(sb: &Sandbox) {
    let start = Instant::now();
    while start.elapsed() < SECS(25) {
        if UnixStream::connect(sb.socket_path(None)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the new daemon never accepted connections\n{}",
        sb.daemon_log(1)
    );
}

#[test]
fn p12_a_killed_daemon_leaves_a_stale_socket_that_the_next_start_replaces() {
    let Some((mut sb, chat)) = running("p12") else {
        skip!("no Rust daemon yet")
    };
    for pid in proc::pids_in(sb.runtime.to_str().unwrap()) {
        if !proc::cmdline(pid).contains("dbus-daemon") {
            proc::kill9(pid);
        }
    }
    thread::sleep(Duration::from_millis(500));
    assert!(
        sb.socket_path(None).exists(),
        "kill -9 should leave the socket behind (that is the scenario)"
    );
    assert!(UnixStream::connect(sb.socket_path(None)).is_err());

    sb.start(&["--background", "--provider", &chat.url()]);
    wait_socket_accepts(&sb);
    expect_ok(&sb, &["--show"]);
    wait_visible(&sb, None, true);
}

#[test]
fn p13_concurrent_commands_all_succeed_and_the_state_file_is_never_torn() {
    let Some((sb, _chat)) = running("p13") else {
        skip!("no Rust daemon yet")
    };
    let torn = std::sync::atomic::AtomicUsize::new(0);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let failures = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok(text) = std::fs::read_to_string(sb.state_path(None)) {
                    if serde_json::from_str::<Value>(&text).is_err() {
                        torn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        });
        let workers: Vec<_> = (0..16)
            .map(|i| {
                let sb = &sb;
                scope.spawn(move || {
                    let flag = ["--show", "--hide", "--toggle", "--background"][i % 4];
                    sb.run(&[flag]).code
                })
            })
            .collect();
        let failures = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|c| *c != 0)
            .count();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        reader.join().unwrap();
        failures
    });
    assert_eq!(failures, 0, "some concurrent commands failed");
    assert_eq!(
        torn.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a reader saw a half-written state file"
    );
}

#[test]
fn p14_an_unreachable_provider_does_not_take_the_daemon_down() {
    let Some(mut sb) = Sandbox::new_or_skip("p14") else {
        return;
    };
    daemon_or_skip!(sb);
    sb.start_hidden("http://127.0.0.1:1/"); // nothing listens there
    thread::sleep(SECS(2));
    expect_ok(&sb, &["--show"]);
    wait_visible(&sb, None, true);
    expect_ok(&sb, &["--hide"]);
    wait_visible(&sb, None, false);
}
