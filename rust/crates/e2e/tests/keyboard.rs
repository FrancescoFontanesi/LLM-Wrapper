//! Tests driven by real keyboard and mouse input (X11 only).
//!
//! They live in their own test binary on purpose: cargo runs binaries one after the other, so no
//! other test maps a window while these type. Without a window manager GTK takes the X input focus
//! whenever any window is presented, so a concurrent test would steal the focus between "focus the
//! window" and "press the key". Inside this binary the tests serialise on `INPUT_LOCK`.

use std::thread;
use std::time::Duration;

use llm_wrapper_e2e::proc;
use llm_wrapper_e2e::{known_defect, skip, FakeChat, Sandbox};

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
        skip!("slow (about 40 s): set LW_SLOW=1");
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
