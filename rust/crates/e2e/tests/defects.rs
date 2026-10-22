//! Known defects of the legacy app ("D" tests). Each test states the behaviour the Rust
//! implementation must have. On the legacy app it is *expected to fail* (`known_defect`), and a
//! legacy pass would be an error, so `docs/spec.md` and this file cannot drift apart.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use llm_wrapper_e2e::proc;
use llm_wrapper_e2e::{known_defect, skip, FakeChat, Sandbox};
use serde_json::Value;

const SECS: fn(u64) -> Duration = Duration::from_secs;

macro_rules! running {
    ($label:expr) => {{
        let Some(mut sb) = Sandbox::new_or_skip($label) else {
            return;
        };
        if !sb.has_daemon() {
            skip!("no Rust daemon built, or no display to open its window");
        }
        let chat = FakeChat::start();
        sb.start_hidden(&chat.url());
        (sb, chat)
    }};
}

fn providers(sb: &Sandbox) -> Value {
    sb.state(None).unwrap()["providers"].clone()
}

#[test]
fn d01_a_refused_command_is_reported_to_the_user() {
    let (sb, _chat) = running!("d01");
    known_defect(
        &sb,
        "D01",
        "the legacy CLI prints OK-and-exit-0 for commands the daemon rejects",
        || {
            let out = sb.run(&["--provider", "no-such-provider"]);
            assert_ne!(out.code, 0, "exit code must be non-zero");
            assert!(
                out.stderr.contains("no-such-provider"),
                "the message must reach the caller's stderr, got {:?}",
                out.stderr
            );
        },
    );
}

#[test]
fn d02_invalid_provider_definitions_are_refused_with_an_error() {
    let (sb, _chat) = running!("d02");
    known_defect(
        &sb,
        "D02",
        "invalid names/URLs are silently ignored by the daemon",
        || {
            let before = providers(&sb);
            for args in [
                &["--add-provider", "bad name", "https://x.example"][..],
                &["--add-provider", "ok", "javascript:alert(1)"],
                &["--add-provider", "ok", "https://user:pw@x.example"],
                &["--remove-provider", "bad name"],
            ] {
                let out = sb.run(args);
                assert_ne!(out.code, 0, "{args:?} must fail");
                assert!(!out.stderr.is_empty(), "{args:?} must say why");
            }
            assert_eq!(providers(&sb), before);
        },
    );
}

#[test]
fn d03_cleartext_http_to_a_remote_host_is_refused() {
    let (sb, _chat) = running!("d03");
    known_defect(
        &sb,
        "D03",
        "any http URL is accepted, sending the session over cleartext",
        || {
            let out = sb.run(&["--add-provider", "lan", "http://192.168.1.10:3000"]);
            assert_ne!(out.code, 0);
            assert!(sb.state(None).unwrap()["providers"].get("lan").is_none());
            // loopback stays allowed
            let out = sb.run(&["--add-provider", "local", "http://127.0.0.1:3000"]);
            assert_eq!(out.code, 0, "{}", out.stderr);
        },
    );
}

#[test]
fn d04_an_invalid_idle_timeout_has_no_side_effects() {
    let (sb, _chat) = running!("d04");
    known_defect(
        &sb,
        "D04",
        "argparse errors are swallowed and the window pops up",
        || {
            let out = sb.run(&["--idle-timeout", "abc"]);
            assert_ne!(out.code, 0);
            thread::sleep(Duration::from_millis(500));
            assert!(
                !sb.visible(None),
                "a rejected command must not show the window"
            );
        },
    );
}

#[test]
fn d05_unknown_options_are_rejected_instead_of_ignored() {
    let (sb, _chat) = running!("d05");
    known_defect(
        &sb,
        "D05",
        "unknown flags and abbreviations are silently accepted",
        || {
            for args in [&["--toogle"][..], &["--tog"], &["stray-word"]] {
                let out = sb.run(args);
                assert_ne!(out.code, 0, "{args:?}");
            }
            thread::sleep(Duration::from_millis(500));
            assert!(!sb.visible(None), "typos must not toggle or show anything");
        },
    );
}

#[test]
fn d06_an_oversized_argument_is_refused_not_silently_dropped() {
    let (sb, _chat) = running!("d06");
    known_defect(
        &sb,
        "D06",
        "payloads above 4096 bytes are truncated and still answered OK",
        || {
            let out = sb.run(&["--user-agent", &"A".repeat(6000)]);
            assert_ne!(out.code, 0);
        },
    );
}

/// Collect what other users could actually reach under `dir`: a path is exposed only if its own
/// mode allows it *and* every directory above it (down from the root being examined) can be
/// traversed. WebKit creates its files with the default umask (0644/0755), so what keeps them
/// private is the `0700` root, and that is exactly what this checks.
fn exposed_below(dir: &Path, reachable: bool, out: &mut Vec<(std::path::PathBuf, u32)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let meta = fs::symlink_metadata(e.path()).unwrap();
        let mode = meta.permissions().mode();
        if reachable && mode & 0o077 != 0 {
            out.push((e.path(), mode));
        }
        if meta.is_dir() {
            exposed_below(&e.path(), reachable && mode & 0o001 != 0, out);
        }
    }
}

#[test]
fn d07_login_data_socket_and_state_are_private_to_the_user() {
    let (sb, _chat) = running!("d07");
    known_defect(
        &sb,
        "D07",
        "session/cache are created 0755/0644 and the socket 0755 (umask 022)",
        || {
            thread::sleep(SECS(2)); // let WebKit create its files
            let mut entries = Vec::new();
            for root in sb.data_dirs() {
                let meta = fs::symlink_metadata(&root)
                    .unwrap_or_else(|_| panic!("{} does not exist", root.display()));
                let mode = meta.permissions().mode();
                // the root itself is judged on its own: the directories above it are normal user directories
                if mode & 0o077 != 0 {
                    entries.push((root.clone(), mode));
                }
                exposed_below(&root, mode & 0o001 != 0, &mut entries);
            }
            for p in [sb.socket_path(None), sb.state_path(None)] {
                entries.push((
                    p.clone(),
                    fs::symlink_metadata(&p).unwrap().permissions().mode(),
                ));
            }
            let open: Vec<String> = entries
                .iter()
                .filter(|(_, m)| m & 0o077 != 0)
                .map(|(p, m)| format!("{} ({:o})", p.display(), m & 0o7777))
                .collect();
            assert!(
                open.is_empty(),
                "readable/writable by other users:\n  {}",
                open.join("\n  ")
            );
        },
    );
}

#[test]
fn d08_a_corrupt_provider_list_is_preserved_not_overwritten() {
    let (mut sb, chat) = running!("d08");
    sb.quit(None);
    assert!(sb.wait_gone(None, SECS(10)));
    let file = sb.providers_file();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "{ \"mine\": \"https://mine.example\", this is not json",
    )
    .unwrap();
    sb.start_hidden(&chat.url());
    known_defect(
        &sb,
        "D08",
        "the corrupt file is replaced by defaults + the new entry without a trace",
        || {
            let out = sb.run(&["--add-provider", "extra", "https://extra.example"]);
            assert_eq!(out.code, 0, "{}", out.stderr);
            sb.wait_state(None, "provider added", |s| {
                s["providers"]["extra"].is_string()
            });
            let dir = file.parent().unwrap();
            let backed_up = fs::read_dir(dir).unwrap().flatten().any(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name != "providers.json"
                    && name.starts_with("providers.json")
                    && fs::read_to_string(e.path()).is_ok_and(|t| t.contains("this is not json"))
            });
            assert!(
                backed_up,
                "the unreadable list must be kept as providers.json.corrupt* before it is replaced"
            );
        },
    );
}

#[test]
fn d09_a_web_process_crash_is_recovered() {
    let Some(mut sb) = Sandbox::new_or_skip("d09") else {
        return;
    };
    if !sb.has_daemon() {
        skip!("no Rust daemon built, or no display to open its window");
    }
    let chat = FakeChat::start();
    let pid = sb.start(&["--background", "--provider", &chat.url()]);
    sb.wait_state(None, "daemon up", |s| s["url"] == chat.url().as_str());
    assert!(chat.wait(SECS(15), |h| !h.is_empty()));
    let victims: Vec<u32> = proc::descendants(pid)
        .into_iter()
        .filter(|p| proc::cmdline(*p).contains("WebKitWebProcess"))
        .collect();
    assert!(
        !victims.is_empty(),
        "no WebKitWebProcess found under the daemon"
    );
    known_defect(
        &sb,
        "D09",
        "after the web process dies the view stays blank forever",
        || {
            let before = chat.total();
            for v in &victims {
                proc::kill9(*v);
            }
            assert!(
                chat.wait(SECS(12), |h| h.len() > before),
                "the page was never reloaded after the crash"
            );
            let out = sb.run(&["--show"]);
            assert_eq!(out.code, 0);
        },
    );
}

#[test]
fn d10_an_idle_connection_does_not_delay_other_commands() {
    let (sb, _chat) = running!("d10");
    known_defect(
        &sb,
        "D10",
        "the IPC listener is serial: an idle connection blocks others for ~1 s",
        || {
            let _idle = UnixStream::connect(sb.socket_path(None)).expect("connect");
            thread::sleep(Duration::from_millis(100));
            let start = Instant::now();
            let out = sb.run(&["--show"]);
            assert_eq!(out.code, 0);
            assert!(
                start.elapsed() < Duration::from_millis(500),
                "took {:?} behind an idle client",
                start.elapsed()
            );
        },
    );
}

#[test]
fn d12_quitting_what_is_not_running_has_no_side_effects() {
    let Some(sb) = Sandbox::new_or_skip("d12") else {
        return;
    };
    known_defect(&sb, "D12", "`--quit` with no daemon imports GTK/WebKit, builds the app and creates the data directories first", || {
        let out = sb.run(&["--quit"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let created: Vec<String> = sb.data_dirs().into_iter().filter(|d| d.exists()).map(|d| d.display().to_string()).collect();
        assert!(created.is_empty(), "--quit created {created:?}");
        assert!(!sb.state_path(None).exists() && !sb.socket_path(None).exists());
    });
}

#[test]
fn d13_cleaning_and_settings_commands_do_not_pop_the_window_up() {
    let (sb, _chat) = running!("d13");
    known_defect(
        &sb,
        "D13",
        "every legacy command ends by showing the window, whatever it was for",
        || {
            for args in [
                &["--soft-clean"][..],
                &["--hard-clean"],
                &["--idle-timeout", "20"],
                &["--user-agent", "E2E-Test/1.0"],
                &["--add-provider", "extra", "https://extra.example"],
                &["--remove-provider", "extra"],
            ] {
                let out = sb.run(args);
                assert_eq!(out.code, 0, "{args:?}: {}", out.stderr);
                thread::sleep(Duration::from_millis(600)); // the legacy daemon acts after replying
                assert!(!sb.visible(None), "{args:?} showed the window");
            }
            // and a visible window stays exactly as it is
            let out = sb.run(&["--show"]);
            assert_eq!(out.code, 0);
            sb.wait_state(None, "window visible", |s| s["visible"] == true);
            for args in [&["--hard-clean"][..], &["--idle-timeout", "25"]] {
                assert_eq!(sb.run(args).code, 0);
                thread::sleep(Duration::from_millis(400));
                assert!(sb.visible(None), "{args:?} hid the window");
            }
        },
    );
}
