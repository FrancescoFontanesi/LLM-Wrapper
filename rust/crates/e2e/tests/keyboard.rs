//! Tests driven by real keyboard and mouse input (X11 only).
//!
//! They live in their own test binary on purpose: cargo runs binaries one after the other, so no
//! other test maps a window while these type. Without a window manager GTK takes the X input focus
//! whenever any window is presented, so a concurrent test would steal the focus between "focus the
//! window" and "press the key". Inside this binary the tests serialise on `INPUT_LOCK`.

use std::thread;
use std::time::Duration;

use llm_wrapper_e2e::proc;
use llm_wrapper_e2e::{known_defect, skip, skip_by_design, FakeChat, Sandbox};

const SECS: fn(u64) -> Duration = Duration::from_secs;

macro_rules! daemon_or_skip {
    ($sb:expr) => {
        if !$sb.has_daemon() {
            skip!("no Rust daemon built, or no display to open its window");
        }
    };
}

fn wait_visible(sb: &Sandbox, want: bool) {
    sb.wait_state(
        None,
        if want {
            "window visible"
        } else {
            "window hidden"
        },
        |s| s["visible"] == want,
    );
}

fn expect_ok(sb: &Sandbox, args: &[&str]) {
    let out = sb.run(args);
    assert_eq!(out.code, 0, "{args:?} failed: {}{}", out.stdout, out.stderr);
}

fn press_until(pid: u32, keys: &str, chat: &FakeChat, path: &str) {
    let before = chat.count(path);
    for _ in 0..4 {
        assert!(proc::press(pid, keys), "no window to type into");
        if chat.wait(SECS(3), |h| {
            h.iter().filter(|x| x.path == path).count() > before
        }) {
            return;
        }
    }
    let seen: Vec<String> = chat.hits().into_iter().map(|h| h.path).collect();
    panic!("{keys} did not produce a request for {path}; the server saw {seen:?}");
}

#[test]
fn k01_assistant_key_bindings() {
    if !proc::have_x11_tools() {
        skip!("xdotool/xprop or an X display are not available");
    }
    let _guard = proc::INPUT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut sb) = Sandbox::new_or_skip("p15") else {
        return;
    };
    daemon_or_skip!(sb);
    let chat = FakeChat::start();
    let pid = sb.start(&["--provider", &chat.url()]);
    wait_visible(&sb, true);
    assert!(chat.wait(SECS(15), |h| !h.is_empty()));
    thread::sleep(SECS(1)); // let the window map and settle

    press_until(pid, "F5", &chat, "/");
    press_until(pid, "ctrl+r", &chat, "/");
    press_until(pid, "ctrl+shift+r", &chat, "/");
    press_until(pid, "ctrl+n", &chat, "/");
    for zoom in ["ctrl+plus", "ctrl+minus", "ctrl+0"] {
        assert!(proc::press(pid, zoom));
    }
    thread::sleep(Duration::from_millis(300));
    assert!(
        sb.visible(None),
        "a zoom key must not hide or kill the window"
    );

    assert!(proc::press(pid, "Escape"));
    wait_visible(&sb, false); // Escape dismisses the assistant
    expect_ok(&sb, &["--show"]);
    wait_visible(&sb, true);
}

#[test]
fn k02_d11_after_a_network_failure_the_page_you_were_on_comes_back() {
    if std::env::var_os("LW_SLOW").is_none() {
        skip_by_design!("slow (about 40 s): set LW_SLOW=1");
    }
    if !proc::have_x11_tools() {
        skip!("xdotool/xprop or an X display are not available");
    }
    let _guard = proc::INPUT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut sb) = Sandbox::new_or_skip("d11") else {
        return;
    };
    if !sb.has_daemon() {
        skip!("no Rust daemon built, or no display to open its window");
    }
    let mut chat = FakeChat::start();
    let pid = sb.start(&["--background", "--provider", &chat.url()]);
    sb.wait_state(None, "daemon up", |s| s["url"] == chat.url().as_str());
    sb.run(&["--add-provider", "site", &chat.url()]);
    sb.run(&["--provider", "site"]);
    sb.wait_state(None, "named provider active", |s| s["active"] == "site");
    thread::sleep(SECS(2));
    // navigate inside the site, like clicking a conversation in the sidebar
    let mut on_conversation = false;
    for _ in 0..4 {
        assert!(proc::click(pid, 100, 100), "no window to click in");
        if chat.wait(SECS(3), |h| h.iter().any(|x| x.path == "/c/abc-123")) {
            on_conversation = true;
            break;
        }
    }
    assert!(
        on_conversation,
        "could not navigate to the conversation (test setup)"
    );

    known_defect(
        &sb,
        "D11",
        "recovery reloads the provider root instead of the page that failed",
        || {
            chat.go_offline();
            assert!(proc::press(pid, "F5"));
            thread::sleep(SECS(2));
            let before = chat.total();
            chat.go_online();
            assert!(
                chat.wait(SECS(45), |h| h.len() > before),
                "no recovery attempt within 45 s of the network coming back"
            );
            thread::sleep(SECS(1));
            let last = chat.last().unwrap().path;
            assert_eq!(last, "/c/abc-123", "recovered to {last}");
        },
    );
}

// ---- Phase 3: what a click can do ----------------------------------------------------------------

/// A visible daemon on `path` of a fresh fake site, with a fake default browser installed. Holds the
/// input lock for as long as the returned guard lives.
macro_rules! desktop {
    ($label:expr, $path:expr) => {{
        if !proc::have_x11_tools() {
            skip!("xdotool/xprop or an X display are not available");
        }
        let guard = proc::INPUT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(mut sb) = Sandbox::new_or_skip($label) else {
            return;
        };
        daemon_or_skip!(sb);
        sb.install_fake_browser();
        let chat = FakeChat::start();
        let url = format!("{}{}", chat.url(), $path);
        let pid = sb.start(&["--provider", &url]);
        sb.wait_state(None, "daemon up", |s| s["url"] == url.as_str());
        assert!(
            chat.wait(SECS(20), |h| !h.is_empty()),
            "the page never loaded"
        );
        let window =
            proc::wait_window(pid, "^127\\.0\\.0\\.1$", SECS(10)).expect("the main window");
        thread::sleep(SECS(1));
        (guard, sb, chat, pid, window)
    }};
}

/// Click the big element at the top of the page until `done` holds (the page may still be loading).
fn click_until(window: &str, done: impl Fn() -> bool) -> bool {
    for _ in 0..5 {
        proc::click_in(window, 100, 100);
        let start = std::time::Instant::now();
        while start.elapsed() < SECS(3) {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    false
}

#[test]
fn k03_a_link_to_another_site_opens_in_the_default_browser_and_leaves_the_window_alone() {
    let (_guard, sb, chat, _pid, window) =
        desktop!("k03", "link?u=http%3A%2F%2Fexternal.test%2Fx%3Fa%3D1");
    assert!(
        click_until(&window, || !sb.browser_opened().is_empty()),
        "the browser was never asked to open anything; daemon: {}",
        sb.daemon_log(0)
    );
    thread::sleep(SECS(1));
    assert_eq!(sb.browser_opened(), ["http://external.test/x?a=1"]);
    assert_eq!(
        chat.count("/link?u=http%3A%2F%2Fexternal.test%2Fx%3Fa%3D1"),
        1,
        "the window navigated"
    );
}

#[test]
fn k04_a_link_that_wants_a_new_window_for_another_site_opens_in_the_default_browser() {
    let (_guard, sb, _chat, _pid, window) =
        desktop!("k04", "blank?u=https%3A%2F%2Fexternal.test%2Fy");
    assert!(
        click_until(&window, || !sb.browser_opened().is_empty()),
        "the browser was never asked to open anything; daemon: {}",
        sb.daemon_log(0)
    );
    thread::sleep(SECS(1));
    assert_eq!(sb.browser_opened(), ["https://external.test/y"]);
}

#[test]
fn k05_links_to_files_mail_and_applications_do_nothing() {
    let (_guard, sb, chat, _pid, window) = desktop!("k05", "link?u=mailto%3Aa%40b.example");
    for (n, target) in [
        "mailto%3Aa%40b.example",
        "file%3A%2F%2F%2Fetc%2Fhostname",
        "javascript%3Awindow.name%3D%27pwned%27",
        "ms-msdt%3A%2Fid",
    ]
    .iter()
    .enumerate()
    {
        if n > 0 {
            let url = format!("{}link?u={target}", chat.url());
            assert_eq!(sb.run(&["--provider", &url]).code, 0);
            sb.wait_state(None, "provider switched", |s| s["url"] == url.as_str());
            thread::sleep(SECS(1));
        }
        let hits = chat.hits().len();
        for _ in 0..3 {
            proc::click_in(&window, 100, 100);
            thread::sleep(SECS(1));
        }
        assert!(
            sb.browser_opened().is_empty(),
            "{target}: opened {:?}",
            sb.browser_opened()
        );
        assert!(sb.visible(None), "{target}: the window went away");
        assert!(chat.hits().len() == hits, "{target}: the page navigated");
    }
    if !sb.is_legacy() {
        let log = sb.daemon_log(0);
        assert!(
            log.contains("refused a navigation to a mailto address"),
            "{log}"
        );
        assert!(
            log.contains("refused a navigation to a ms-msdt address"),
            "{log}"
        );
    }
    // `file:` from a web page, and `javascript:`, never reach the app: WebKit settles them itself
}

#[test]
fn k06_a_permission_asked_with_a_click_reaches_the_app_and_is_denied() {
    let (_guard, sb, chat, _pid, window) = desktop!("k06", "perm-click");
    assert!(
        click_until(&window, || chat
            .hits()
            .iter()
            .any(|h| h.path.starts_with("/report-click?"))),
        "the page never got an answer; daemon: {}",
        sb.daemon_log(0)
    );
    let answer = chat
        .hits()
        .iter()
        .find(|h| h.path.starts_with("/report-click?"))
        .unwrap()
        .query();
    assert_eq!(answer, "denied");
    if !sb.is_legacy() {
        let log = sb.daemon_log(0);
        assert!(
            log.contains("denied a permission request"),
            "the app never saw it: {log}"
        );
    }
}

#[test]
fn k07_a_download_is_saved_where_the_user_says_and_announced() {
    if !Sandbox::can_watch_notifications() && std::env::var("LW_IMPL").as_deref() == Ok("rust") {
        skip!("dbus-monitor (package dbus-bin) is needed to see the notification");
    }
    let (_guard, mut sb, _chat, pid, window) = desktop!("k07", "link?u=%2Fdownload");
    if !sb.is_legacy() {
        sb.watch_notifications();
    }
    let dest = std::env::temp_dir().join(format!("lw-k07-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&dest);
    proc::click_in(&window, 100, 100);
    assert!(
        proc::wait_window(pid, "Save File", SECS(15)).is_some(),
        "no save dialog; daemon: {}",
        sb.daemon_log(0)
    );
    thread::sleep(SECS(1));
    // the name offered is the file name the server suggested, stripped of the directories it named
    proc::press_focused("ctrl+a");
    proc::type_focused(&dest.display().to_string());
    proc::press_focused("Return");
    for _ in 0..50 {
        if dest.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    thread::sleep(SECS(1));
    let saved = std::fs::read_to_string(&dest);
    let _ = std::fs::remove_file(&dest);
    assert_eq!(
        saved.as_deref().ok(),
        Some("payload-bytes"),
        "daemon: {}",
        sb.daemon_log(0)
    );
    // and the desktop is told, by name, without markup (the legacy app runs `notify-send`)
    if !sb.is_legacy() {
        let name = dest.file_name().unwrap().to_string_lossy().into_owned();
        for _ in 0..50 {
            if sb.notifications_seen().contains("Download complete") {
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
        let seen = sb.notifications_seen();
        assert!(seen.contains("llm-wrapper"), "{seen}");
        assert!(seen.contains("Download complete"), "{seen}");
        assert!(seen.contains(&format!("Saved {name}")), "{seen}");
    }
}

#[test]
fn k08_the_user_picks_the_file_a_page_asks_for_and_the_page_gets_only_that() {
    let (_guard, sb, chat, pid, window) = desktop!("k08", "upload");
    if sb.is_legacy() {
        skip_by_design!("the legacy GTK 3 chooser needs different keys");
    }
    let file = std::env::temp_dir().join(format!("lw-k08-{}.txt", std::process::id()));
    std::fs::write(&file, "hello upload").unwrap();
    proc::click_in(&window, 100, 100);
    assert!(
        proc::wait_window(pid, "Select File to Upload", SECS(15)).is_some(),
        "no file dialog; daemon: {}",
        sb.daemon_log(0)
    );
    thread::sleep(SECS(1));
    proc::press_focused("ctrl+l");
    proc::type_focused(&file.display().to_string());
    // the entry completes what is typed ("/t" -> "/tmp/") and leaves the guess selected after the
    // cursor: delete it, or it becomes part of the name
    proc::press_focused("shift+End");
    proc::press_focused("Delete");
    proc::press_focused("Return");
    let got = chat.wait(SECS(15), |h| {
        h.iter().any(|h| h.path.starts_with("/uploaded?"))
    });
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let seen = chat
        .hits()
        .iter()
        .find(|h| h.path.starts_with("/uploaded?"))
        .map(llm_wrapper_e2e::fakechat::Hit::query);
    let _ = std::fs::remove_file(&file);
    assert!(
        got,
        "the page never received the file; daemon: {}",
        sb.daemon_log(0)
    );
    assert_eq!(seen, Some(format!("{name}:12")));
}

#[test]
fn k09_cancelling_the_dialog_cancels_the_download() {
    let (_guard, sb, chat, pid, window) = desktop!("k09", "link?u=%2Fdownload");
    proc::click_in(&window, 100, 100);
    assert!(proc::wait_window(pid, "Save File", SECS(15)).is_some());
    thread::sleep(SECS(1));
    proc::press_focused("Escape");
    thread::sleep(SECS(2));
    assert!(
        proc::windows_named(pid, "Save File").is_empty(),
        "the dialog is still there"
    );
    // the app carries on: the page is still there and takes a new command
    assert!(sb.visible(None));
    assert_eq!(sb.run(&["--hide"]).code, 0);
    assert!(chat.count("/download") <= 1);
}

#[test]
fn k10_one_click_opens_the_browser_once() {
    let (_guard, sb, _chat, _pid, window) = desktop!("k10", "open3");
    if sb.is_legacy() {
        skip_by_design!("the legacy app opens the browser once per window request");
    }
    assert!(
        click_until(&window, || !sb.browser_opened().is_empty()),
        "the browser was never asked to open anything; daemon: {}",
        sb.daemon_log(0)
    );
    thread::sleep(SECS(2));
    assert_eq!(sb.browser_opened(), ["http://external.test/1"]);
    assert!(
        sb.daemon_log(0).contains("too many in a row"),
        "{}",
        sb.daemon_log(0)
    );
}
