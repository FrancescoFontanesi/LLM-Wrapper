//! Baseline measurements of an implementation, through the same black-box sandbox as the tests.
//!
//!   bench [--reps N] [--out FILE]        (implementation chosen with LW_IMPL, as for the tests)
//!
//! Reports: cold start, hotkey latency (command spawned -> state file shows the new visibility),
//! memory (PSS) per process role while hidden and while visible, and CPU use while hidden.
//! Run under a private X server (see `scripts/e2e.sh --bench`). Absolute numbers depend on the
//! machine and on software rendering; compare implementations under the *same* conditions.

use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use llm_wrapper_e2e::proc;
use llm_wrapper_e2e::{FakeChat, Sandbox};
use serde_json::{json, Value};

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

fn summary(mut v: Vec<f64>) -> Value {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({"n": v.len(), "min": v[0], "p50": pct(&v, 50.0), "p90": pct(&v, 90.0), "p99": pct(&v, 99.0), "max": v[v.len() - 1]})
}

fn role(pid: u32, daemon_key: &str) -> Option<&'static str> {
    let c = proc::cmdline(pid);
    Some(if c.contains("WebKitWebProcess") {
        "web"
    } else if c.contains("WebKitNetworkProcess") {
        "network"
    } else if c.contains("dbus-daemon") {
        return None;
    } else if c.contains("bwrap") || c.contains("xdg-dbus-proxy") {
        "sandbox_helpers"
    } else if c.contains(daemon_key) {
        "ui"
    } else {
        "other"
    })
}

/// Memory by process role: `<role>` is proportional resident size (shared pages split between their
/// users); `<role>_anon` is the part that is private heap, which only exiting returns — the rest is
/// mostly shared libraries and the page cache, clean pages the kernel takes back when it needs them.
fn memory(sb: &Sandbox) -> Value {
    let key = if sb.is_legacy() {
        "app.py"
    } else {
        "llm-wrapperd"
    };
    let mut pss = std::collections::BTreeMap::<&str, u64>::new();
    let mut anon = std::collections::BTreeMap::<&str, u64>::new();
    for pid in proc::pids_in(sb.runtime.to_str().unwrap()) {
        if let Some(r) = role(pid, key) {
            *pss.entry(r).or_default() += proc::pss_kib(pid);
            *anon.entry(r).or_default() += proc::smaps_field_kib(pid, "Pss_Anon");
        }
    }
    let mut out = serde_json::Map::new();
    for (r, kib) in &pss {
        out.insert((*r).to_owned(), json!(kib / 1024));
        out.insert(format!("{r}_anon"), json!(anon[r] / 1024));
    }
    out.insert("total_mib".into(), json!(pss.values().sum::<u64>() / 1024));
    out.insert(
        "total_anon_mib".into(),
        json!(anon.values().sum::<u64>() / 1024),
    );
    Value::Object(out)
}

fn cpu_ticks(sb: &Sandbox) -> u64 {
    proc::pids_in(sb.runtime.to_str().unwrap())
        .into_iter()
        .filter(|p| role(*p, "").is_some())
        .map(proc::cpu_ticks)
        .sum()
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn expect_ok(sb: &Sandbox, args: &[&str]) {
    let o = sb.run(args);
    assert_eq!(o.code, 0, "{args:?}: {}", o.stderr);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str, default: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    };
    let reps: usize = flag("--reps", "40").parse().expect("--reps N");
    let out_file = flag("--out", "");
    let clk: f64 = sh("getconf", &["CLK_TCK"]).parse().unwrap_or(100.0);

    // ---- cold start ------------------------------------------------------------------------
    let mut cold = Vec::new();
    for _ in 0..5 {
        let mut sb = Sandbox::new("bcold");
        let chat = FakeChat::start();
        let t0 = Instant::now();
        sb.start(&["--background", "--provider", &chat.url()]);
        sb.wait_state(None, "daemon up", |s| s["url"] == chat.url().as_str());
        cold.push(ms(t0.elapsed()));
    }

    // ---- steady state --------------------------------------------------------------------------
    let mut sb = Sandbox::new("bsteady");
    let chat = FakeChat::start();
    // `--page heavy`: a page holding ~150 MiB, like a long conversation on a real site
    let page = flag("--page", "");
    let url = format!("{}{page}", chat.url());
    sb.start_hidden(&url);
    assert!(
        chat.wait(Duration::from_secs(20), |h| !h.is_empty()),
        "page never loaded"
    );
    thread::sleep(Duration::from_secs(5));
    let mem_hidden = memory(&sb);

    let (t0, ticks0) = (Instant::now(), cpu_ticks(&sb));
    thread::sleep(Duration::from_secs(10));
    let cpu_pct_hidden =
        (cpu_ticks(&sb) - ticks0) as f64 / clk / t0.elapsed().as_secs_f64() * 100.0;

    // ---- hotkey latency ------------------------------------------------------------------------
    let (mut wall, mut to_state) = (Vec::new(), Vec::new());
    for _ in 0..reps {
        let was = sb.visible(None);
        let t0 = Instant::now();
        let o = sb.run(&["--toggle"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        wall.push(ms(t0.elapsed()));
        while sb.visible(None) == was {
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "toggle never took effect"
            );
            thread::sleep(Duration::from_micros(300));
        }
        to_state.push(ms(t0.elapsed()));
        thread::sleep(Duration::from_millis(120));
    }
    if !sb.visible(None) {
        sb.run(&["--show"]);
    }
    thread::sleep(Duration::from_secs(4));
    let mem_visible = memory(&sb);

    // ---- hidden and idle: what the memory policy is for -----------------------------------------
    // --idle-timeout 1 is the shortest the command line allows; the wait covers it plus the check.
    let idle_wait: u64 = flag("--idle-wait", "80")
        .parse()
        .expect("--idle-wait SECONDS");
    let (mut mem_idle, mut wake) = (Value::Null, Value::Null);
    if idle_wait > 0 {
        expect_ok(&sb, &["--idle-timeout", "1"]);
        expect_ok(&sb, &["--hide"]);
        sb.wait_state(None, "window hidden", |s| s["visible"] == false);
        thread::sleep(Duration::from_secs(idle_wait));
        mem_idle = memory(&sb);
        // summon it again: how long until the window is up, and until the page is asked for
        let requests = chat.total();
        let t0 = Instant::now();
        expect_ok(&sb, &["--show"]);
        while !sb.visible(None) {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "never became visible"
            );
            thread::sleep(Duration::from_micros(300));
        }
        let visible_ms = ms(t0.elapsed());
        let request_ms = chat
            .wait(Duration::from_secs(10), |h| h.len() > requests)
            .then(|| ms(t0.elapsed()));
        wake = json!({"window_visible_ms": visible_ms, "page_requested_ms": request_ms});
    }

    let report = json!({
        "implementation": if sb.is_legacy() { "legacy (python, GTK3, WebKitGTK 4.1)" } else { "rust" },
        "conditions": {
            "kernel": sh("uname", &["-r"]),
            "cpu": fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_owned())),
            "cores": thread::available_parallelism().map(usize::from).unwrap_or(0),
            "webkit": sh("dpkg-query", &["-W", "-f=${Version}", "libwebkit2gtk-4.1-0"]),
            "display": "Xvfb (software rendering), fake chat page on loopback",
            "page": if page.is_empty() { "light (a few KiB)" } else { page.as_str() },
        },
        "cold_start_ms": summary(cold),
        "hotkey_ms": {
            "launcher_wall": summary(wall),
            "command_to_state_visible_flip": summary(to_state),
        },
        "memory_pss_mib": {"hidden": mem_hidden, "visible": mem_visible, "hidden_and_idle": mem_idle},
        "wake_after_idle_ms": wake,
        "cpu_percent_while_hidden_10s": (cpu_pct_hidden * 100.0).round() / 100.0,
    });
    let text = serde_json::to_string_pretty(&report).unwrap();
    println!("{text}");
    if !out_file.is_empty() {
        fs::write(&out_file, text + "\n").expect("write --out");
    }
}
