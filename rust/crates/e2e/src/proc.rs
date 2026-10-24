//! Looking at processes through `/proc`, and X11 helpers (`xdotool`, `xprop`) for window class and keys.

use std::fs;
use std::process::Command;
use std::sync::Mutex;

/// All pids whose environment has `XDG_RUNTIME_DIR=<runtime>`: the daemon, its WebKit helpers and
/// anything else started inside one sandbox.
pub fn pids_in(runtime: &str) -> Vec<u32> {
    let needle = format!("XDG_RUNTIME_DIR={runtime}\0");
    let me = std::process::id();
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(env) = fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if String::from_utf8_lossy(&env).contains(&needle) {
            out.push(pid);
        }
    }
    out
}

pub fn cmdline(pid: u32) -> String {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            String::from_utf8_lossy(&b)
                .replace('\0', " ")
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

pub fn parent(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

pub fn alive(pid: u32) -> bool {
    // a zombie is not alive for our purposes
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .map(|(_, r)| r.trim_start().starts_with(|c| c != 'Z' && c != 'X'))
        })
        .unwrap_or(false)
}

/// Every descendant of `root` (children, grandchildren, ...), found by walking `/proc`.
pub fn descendants(root: u32) -> Vec<u32> {
    let mut all = Vec::new();
    if let Ok(dir) = fs::read_dir("/proc") {
        for e in dir.flatten() {
            if let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
                if let Some(pp) = parent(pid) {
                    all.push((pid, pp));
                }
            }
        }
    }
    let mut found = vec![];
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for (pid, pp) in &all {
            if *pp == p && !found.contains(pid) {
                found.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    found
}

pub fn kill9(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
}

/// Resident set the kernel attributes to this process alone (proportional share of shared pages).
pub fn pss_kib(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Pss:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// One field of `smaps_rollup` (`Pss_Anon`, `Pss_File`, `Pss_Shmem`...) in KiB.
pub fn smaps_field_kib(pid: u32, field: &str) -> u64 {
    let key = format!("{field}:");
    fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(&key))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// Clock ticks (user+system) the process has used so far.
pub fn cpu_ticks(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            let f: Vec<&str> = s.rsplit_once(')')?.1.split_whitespace().collect();
            Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
        })
        .unwrap_or(0)
}

// ---- X11 ---------------------------------------------------------------------------------------

/// Keyboard input goes to whatever has the X focus, which is global: tests that type must not
/// overlap. They hold this lock.
pub static INPUT_LOCK: Mutex<()> = Mutex::new(());

pub fn have_x11_tools() -> bool {
    std::env::var_os("DISPLAY").is_some()
        && ["xdotool", "xprop"].iter().all(|t| {
            Command::new(t).arg("--version").output().is_ok()
                || Command::new(t).arg("-version").output().is_ok()
        })
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Does GTK believe it runs in a sandbox? It does when `/.flatpak-info` exists (a flatpak, or a host
/// where bubblewrap run as root left the file behind as a mount point), and then it sets no
/// `_NET_WM_PID` on its windows, so windows can no longer be matched to a process.
fn gtk_thinks_it_is_sandboxed() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Mapped windows of the daemon class (`llm-wrapper`, `llm-wrapper-NAME`), whatever their process.
/// Only a fallback: the suites that look at windows run one daemon at a time.
fn windows_by_class() -> Vec<String> {
    run(
        "xdotool",
        &["search", "--onlyvisible", "--class", "^llm-wrapper"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

fn windows_of_pid(pid: u32) -> Vec<String> {
    run("xdotool", &["search", "--pid", &pid.to_string()])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// X window ids owned by `pid`; where windows carry no process id, the mapped windows of the daemon
/// class (right for the suites that drive one daemon at a time).
pub fn windows_of(pid: u32) -> Vec<String> {
    let owned = windows_of_pid(pid);
    if owned.is_empty() && gtk_thinks_it_is_sandboxed() {
        return windows_by_class();
    }
    owned
}

/// `WM_CLASS` of the first window of `pid` that has a class: (instance, class).
pub fn wm_class(pid: u32) -> Option<(String, String)> {
    for id in windows_of_pid(pid) {
        let out = run("xprop", &["-id", &id, "WM_CLASS"]);
        if let Some((_, rhs)) = out.split_once('=') {
            let parts: Vec<String> = rhs
                .split(',')
                .map(|p| p.trim().trim_matches('"').to_owned())
                .collect();
            // GTK also creates an invisible group-leader window whose class is empty: skip it
            if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                return Some((parts[0].clone(), parts[1].clone()));
            }
        }
    }
    None
}

/// Like [`wm_class`], for tests that run alongside others: where windows carry no process id, a
/// mapped window whose class is exactly `expected` stands in (the tests give each instance a class
/// of its own).
pub fn wm_class_expecting(pid: u32, expected: &str) -> Option<(String, String)> {
    wm_class(pid).or_else(|| {
        if !gtk_thinks_it_is_sandboxed() {
            return None;
        }
        let pattern = format!("^{}$", expected.replace('-', "\\-"));
        let found = !run("xdotool", &["search", "--onlyvisible", "--class", &pattern])
            .trim()
            .is_empty();
        found.then(|| (expected.to_owned(), expected.to_owned()))
    })
}

/// Focus the window of `pid`, put the pointer over it, and press `keys` (xdotool syntax).
pub fn press(pid: u32, keys: &str) -> bool {
    let Some(id) = windows_of(pid)
        .into_iter()
        .find(|w| run("xdotool", &["getwindowgeometry", w]).contains("Geometry"))
    else {
        return false;
    };
    run("xdotool", &["windowfocus", &id]);
    run("xdotool", &["mousemove", "--window", &id, "120", "120"]);
    run("xdotool", &["key", "--clearmodifiers", keys]);
    true
}

/// Click at (x, y) inside the window of `pid`.
pub fn click(pid: u32, x: u32, y: u32) -> bool {
    let Some(id) = windows_of(pid).into_iter().next() else {
        return false;
    };
    run("xdotool", &["windowfocus", &id]);
    run(
        "xdotool",
        &[
            "mousemove",
            "--window",
            &id,
            &x.to_string(),
            &y.to_string(),
            "click",
            "1",
        ],
    );
    true
}

/// Ids of the mapped windows of `pid` whose name matches `pattern` (a regular expression).
pub fn windows_named(pid: u32, pattern: &str) -> Vec<String> {
    let found = windows_named_by_pid(pid, pattern);
    if found.is_empty() && gtk_thinks_it_is_sandboxed() {
        return windows_titled(pattern);
    }
    found
}

fn windows_named_by_pid(pid: u32, pattern: &str) -> Vec<String> {
    run(
        "xdotool",
        &[
            "search",
            "--all", // xdotool's default is "any": every window of the process would match
            "--onlyvisible",
            "--pid",
            &pid.to_string(),
            "--name",
            pattern,
        ],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// Ids of the mapped windows, of any process, whose name matches `pattern`.
pub fn windows_titled(pattern: &str) -> Vec<String> {
    run("xdotool", &["search", "--onlyvisible", "--name", pattern])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Wait up to `limit` for a mapped window of `pid` named like `pattern`; its id.
pub fn wait_window(pid: u32, pattern: &str, limit: std::time::Duration) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if let Some(id) = windows_named(pid, pattern).into_iter().next() {
            return Some(id);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    None
}

/// Focus window `id` and click at (x, y) inside it.
pub fn click_in(id: &str, x: u32, y: u32) {
    run("xdotool", &["windowfocus", id]);
    run(
        "xdotool",
        &[
            "mousemove",
            "--window",
            id,
            &x.to_string(),
            &y.to_string(),
            "click",
            "1",
        ],
    );
}

/// Press `keys` (xdotool syntax) in whatever has the input focus. A dialog the app has just opened
/// has it already; moving it with `windowfocus` makes GTK treat the dialog as inactive.
pub fn press_focused(keys: &str) {
    run("xdotool", &["key", "--clearmodifiers", keys]);
}

/// Type `text` into whatever has the input focus.
pub fn type_focused(text: &str) {
    run(
        "xdotool",
        &["type", "--clearmodifiers", "--delay", "20", text],
    );
}
