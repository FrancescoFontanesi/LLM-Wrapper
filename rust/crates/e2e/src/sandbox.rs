//! One isolated world per test: its own directories, XDG variables, private D-Bus session bus and
//! runtime directory, plus the knowledge of where each implementation keeps things.
//!
//! The suite is *black box*: it only uses the command line, the state file, the socket, the
//! window (X11) and the files on disk. `LW_IMPL=legacy` (default) drives the Python app copied from
//! the repository; `LW_IMPL=rust` drives the Rust client and daemon from `LW_RUST_BIN`.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::proc;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impl {
    Legacy,
    Rust,
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

pub struct Sandbox {
    pub imp: Impl,
    pub root: PathBuf,
    pub runtime: PathBuf,
    launcher: PathBuf,
    rust_bin: Option<PathBuf>,
    bus: Option<Child>,
    daemons: Vec<(Option<String>, Child, PathBuf)>,
    /// Observers (a bus monitor...) that must not outlive the sandbox.
    helpers: Vec<Child>,
    keep: bool,
}

/// Print a SKIP line and return from the test: this implementation has nothing to check here by
/// design (a Rust-only property run on the legacy app, a deliberate change, an opt-in slow test).
/// Unlike [`skip!`] it is not a failure under `LW_STRICT=1`.
#[macro_export]
macro_rules! skip_by_design {
    ($($arg:tt)*) => {{
        eprintln!("SKIP: {}", format!($($arg)*));
        return;
    }};
}

/// Print a SKIP line and return from the test, or fail when `LW_STRICT=1`: something the suite
/// needs (a daemon, a display, a tool) is missing.
#[macro_export]
macro_rules! skip {
    ($($arg:tt)*) => {{
        let why = format!($($arg)*);
        if std::env::var_os("LW_STRICT").is_some() {
            panic!("LW_STRICT: would skip: {why}");
        }
        eprintln!("SKIP: {why}");
        return;
    }};
}

fn spawn_retrying(cmd: &mut Command) -> Child {
    // ETXTBSY: another test thread forked while a freshly copied script was still open for writing.
    for _ in 0..100 {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(26) => thread::sleep(Duration::from_millis(10)),
            other => return other.expect("spawn"),
        }
    }
    panic!("ETXTBSY persisted");
}

fn python_with_gi() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LW_PYTHON") {
        return Some(p.into());
    }
    [
        "/usr/bin/python3.12",
        "/usr/bin/python3.11",
        "/usr/bin/python3.13",
        "/usr/bin/python3",
    ]
    .into_iter()
    .find(|cand| {
        Command::new(cand)
            .args(["-c", "import gi"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
    .map(PathBuf::from)
}

fn first_python_with_gi() -> PathBuf {
    python_with_gi().expect("no python with PyGObject found; set LW_PYTHON")
}

fn has_display() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn rust_bin_dir() -> PathBuf {
    std::env::var_os("LW_RUST_BIN").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release"),
        PathBuf::from,
    )
}

impl Sandbox {
    /// Like [`Sandbox::new`] but prints `SKIP: why` and returns `None` when the environment cannot
    /// run the suite (no X display, no `dbus-daemon`, no PyGObject for the legacy app, ...).
    /// With `LW_STRICT=1` a missing prerequisite is a failure instead.
    pub fn new_or_skip(label: &str) -> Option<Self> {
        match Self::preconditions() {
            Ok(()) => Some(Self::new(label)),
            Err(why) => {
                assert!(std::env::var_os("LW_STRICT").is_none(), "LW_STRICT: {why}");
                eprintln!("SKIP: {why}");
                None
            }
        }
    }

    fn preconditions() -> Result<(), String> {
        if Command::new("dbus-daemon")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_err()
        {
            return Err(
                "dbus-daemon is not installed (each test runs on a private session bus)".into(),
            );
        }
        if std::env::var("LW_IMPL").as_deref() == Ok("rust") {
            let bin = rust_bin_dir();
            if !bin.join("llm-wrapper").exists() {
                return Err(format!("{}/llm-wrapper not found: build it (scripts/build-release.sh) and set LW_RUST_BIN", bin.display()));
            }
            return Ok(());
        }
        if !has_display() {
            return Err("no X display: run under xvfb-run (scripts/e2e.sh does)".into());
        }
        if python_with_gi().is_none() {
            return Err("no python3 with PyGObject for the legacy app (set LW_PYTHON)".into());
        }
        Ok(())
    }

    pub fn new(label: &str) -> Self {
        let imp = match std::env::var("LW_IMPL").as_deref() {
            Ok("rust") => Impl::Rust,
            _ => Impl::Legacy,
        };
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let label: String = label
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(10)
            .collect();
        let root = PathBuf::from(format!("/tmp/lwe-{}-{n}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for sub in [
            "",
            "rt",
            "home",
            "xdg/data",
            "xdg/cache",
            "xdg/config",
            "xdg/state",
        ] {
            let p = root.join(sub);
            fs::create_dir_all(&p).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        }

        let (launcher, rust_bin) = match imp {
            Impl::Legacy => {
                let src = std::env::var_os("LW_LEGACY_SRC").map_or_else(
                    || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
                    PathBuf::from,
                );
                let dir = root.join("legacy");
                fs::create_dir_all(dir.join(".venv/bin")).unwrap();
                for f in ["app.py", "llm-wrapper"] {
                    fs::copy(src.join(f), dir.join(f))
                        .unwrap_or_else(|e| panic!("copy {f} from {}: {e}", src.display()));
                }
                fs::set_permissions(dir.join("llm-wrapper"), fs::Permissions::from_mode(0o755))
                    .unwrap();
                std::os::unix::fs::symlink(first_python_with_gi(), dir.join(".venv/bin/python"))
                    .unwrap();
                (dir.join("llm-wrapper"), None)
            }
            Impl::Rust => {
                let bin = rust_bin_dir();
                (bin.join("llm-wrapper"), Some(bin))
            }
        };

        let mut sb = Self {
            imp,
            runtime: root.join("rt"),
            root,
            launcher,
            rust_bin,
            bus: None,
            daemons: Vec::new(),
            helpers: Vec::new(),
            keep: std::env::var_os("LW_KEEP").is_some(),
        };
        sb.start_bus();
        sb
    }

    fn start_bus(&mut self) {
        let sock = self.root.join("bus");
        let conf = self.root.join("bus.conf");
        fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\
                 <busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                sock.display()
            ),
        )
        .unwrap();
        let mut cmd = Command::new("dbus-daemon");
        cmd.arg(format!("--config-file={}", conf.display()))
            .arg("--nofork");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.env("XDG_RUNTIME_DIR", &self.runtime); // so the safety-net sweep finds it too
        self.bus = Some(
            cmd.spawn()
                .expect("dbus-daemon is required for the e2e suite"),
        );
        let start = Instant::now();
        while !sock.exists() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "private D-Bus did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn is_legacy(&self) -> bool {
        self.imp == Impl::Legacy
    }

    /// Whether a daemon (and a display for its window) is available: tests that need one skip otherwise.
    pub fn has_daemon(&self) -> bool {
        match &self.rust_bin {
            Some(b) => b.join("llm-wrapperd").exists() && has_display(),
            None => true,
        }
    }

    // ---- where things live ------------------------------------------------------------------

    pub fn state_path(&self, instance: Option<&str>) -> PathBuf {
        self.runtime.join(match instance {
            Some(i) => format!("llm-wrapper-{i}.json"),
            None => "llm-wrapper.json".to_owned(),
        })
    }

    pub fn socket_path(&self, instance: Option<&str>) -> PathBuf {
        let suffix = if self.is_legacy() { "sock" } else { "v1.sock" };
        self.runtime.join(match instance {
            Some(i) => format!("llm-wrapper-{i}.{suffix}"),
            None => format!("llm-wrapper.{suffix}"),
        })
    }

    /// Directories holding login cookies, caches and configuration.
    pub fn data_dirs(&self) -> Vec<PathBuf> {
        match self.imp {
            Impl::Legacy => vec![
                self.root.join("legacy/session"),
                self.root.join("legacy/cache"),
            ],
            Impl::Rust => vec![
                self.root.join("xdg/data/llm-wrapper"),
                self.root.join("xdg/cache/llm-wrapper"),
                self.root.join("xdg/config/llm-wrapper"),
            ],
        }
    }

    pub fn providers_file(&self) -> PathBuf {
        match self.imp {
            Impl::Legacy => self.root.join("legacy/providers.json"),
            Impl::Rust => self.root.join("xdg/config/llm-wrapper/providers.json"),
        }
    }

    // ---- the desktop around the app --------------------------------------------------------------

    /// Make a fake default browser for `http` and `https` links: a program that appends the address
    /// it is given to a file. Install it before the daemon starts.
    pub fn install_fake_browser(&self) {
        let script = self.root.join("fake-browser.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$1\" >> '{}'\n",
                self.browser_log().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let apps = self.root.join("xdg/data/applications");
        fs::create_dir_all(&apps).unwrap();
        fs::write(
            apps.join("fake-browser.desktop"),
            format!(
                "[Desktop Entry]\nType=Application\nName=Fake Browser\nExec={} %u\n\
                 MimeType=x-scheme-handler/http;x-scheme-handler/https;\nNoDisplay=true\n",
                script.display()
            ),
        )
        .unwrap();
        let config = self.root.join("xdg/config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/http=fake-browser.desktop\n\
             x-scheme-handler/https=fake-browser.desktop\n",
        )
        .unwrap();
    }

    /// Record the desktop notifications the app sends, by spying on the private session bus.
    pub fn watch_notifications(&mut self) {
        let f = fs::File::create(self.notification_log()).unwrap();
        let mut cmd = self.command_for(Path::new("dbus-monitor"));
        cmd.args([
            "--address".to_owned(),
            format!("unix:path={}", self.root.join("bus").display()),
            "type='method_call',interface='org.freedesktop.Notifications',member='Notify'"
                .to_owned(),
        ])
        .stdout(f)
        .stderr(Stdio::null());
        self.helpers.push(
            cmd.spawn()
                .expect("dbus-monitor is needed to watch notifications"),
        );
        thread::sleep(Duration::from_millis(500)); // let it attach to the bus
    }

    fn notification_log(&self) -> PathBuf {
        self.root.join("notifications.log")
    }

    /// What the notification watcher has recorded so far (`dbus-monitor`'s text).
    pub fn notifications_seen(&self) -> String {
        fs::read_to_string(self.notification_log()).unwrap_or_default()
    }

    fn browser_log(&self) -> PathBuf {
        self.root.join("browser.log")
    }

    /// Addresses handed to the fake default browser so far.
    pub fn browser_opened(&self) -> Vec<String> {
        fs::read_to_string(self.browser_log())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    // ---- running things -----------------------------------------------------------------------

    fn base_command(&self, args: &[&str]) -> Command {
        let mut cmd = self.command_for(&self.launcher);
        cmd.args(args);
        cmd
    }

    fn command_for(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear();
        for key in [
            "PATH",
            "DISPLAY",
            "XAUTHORITY",
            "WAYLAND_DISPLAY",
            "LANG",
            "LD_LIBRARY_PATH",
        ] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("HOME", self.root.join("home"))
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_DATA_HOME", self.root.join("xdg/data"))
            .env("XDG_CACHE_HOME", self.root.join("xdg/cache"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg/config"))
            .env("XDG_STATE_HOME", self.root.join("xdg/state"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", self.root.join("bus").display()),
            )
            .env("NO_AT_BRIDGE", "1")
            .env("GTK_A11Y", "none")
            .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
            .env("GSK_RENDERER", "cairo")
            .stdin(Stdio::null());
        cmd
    }

    /// Run the launcher to completion (it returns quickly when a daemon is running).
    pub fn run(&self, args: &[&str]) -> Out {
        self.run_os(
            args.iter().map(OsString::from).collect(),
            Duration::from_secs(25),
        )
    }

    pub fn run_os(&self, args: Vec<OsString>, limit: Duration) -> Out {
        self.run_program(&self.launcher.clone(), args, limit)
    }

    /// Run `llm-wrapperd` itself (Rust only), the way the client does when no daemon runs. With a
    /// daemon already running it must hand the command over and exit.
    pub fn run_daemon_binary(&self, args: &[&str]) -> Out {
        let bin = self
            .rust_bin
            .as_ref()
            .expect("the Rust implementation only")
            .join("llm-wrapperd");
        self.run_program(
            &bin,
            args.iter().map(OsString::from).collect(),
            Duration::from_secs(25),
        )
    }

    fn run_program(&self, program: &Path, args: Vec<OsString>, limit: Duration) -> Out {
        let mut cmd = self.command_for(program);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let start = Instant::now();
        let mut child = spawn_retrying(&mut cmd);
        let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let t1 = thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let t2 = thread::spawn(move || {
            let mut s = String::new();
            let _ = se.read_to_string(&mut s);
            s
        });
        let status = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                panic!("launcher did not finish within {limit:?}");
            }
            thread::sleep(Duration::from_millis(3));
        };
        Out {
            code: status.code().unwrap_or(-1),
            stdout: t1.join().unwrap(),
            stderr: t2.join().unwrap(),
            elapsed: start.elapsed(),
        }
    }

    /// Start a long-lived process (the daemon runs in the foreground of the launcher). Returns its pid.
    pub fn start(&mut self, args: &[&str]) -> u32 {
        let instance = args
            .iter()
            .position(|a| *a == "--instance")
            .and_then(|i| args.get(i + 1))
            .map(|s| (*s).to_owned())
            .or_else(|| {
                args.iter()
                    .find_map(|a| a.strip_prefix("--instance=").map(str::to_owned))
            });
        let log = self.root.join(format!("daemon-{}.log", self.daemons.len()));
        let f = fs::File::create(&log).unwrap();
        let mut cmd = self.base_command(args);
        cmd.stdout(f.try_clone().unwrap()).stderr(f);
        let child = spawn_retrying(&mut cmd);
        let pid = child.id();
        self.daemons.push((instance, child, log));
        pid
    }

    pub fn daemon_log(&self, index: usize) -> String {
        self.daemons
            .get(index)
            .map(|(_, _, p)| fs::read_to_string(p).unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn daemon_exited(&mut self, index: usize) -> bool {
        self.daemon_exit_code(index).is_some()
    }

    /// Exit code of the `index`-th process started with [`Sandbox::start`], once it has exited
    /// (`-1` when a signal ended it).
    pub fn daemon_exit_code(&mut self, index: usize) -> Option<i32> {
        self.daemons[index]
            .1
            .try_wait()
            .unwrap()
            .map(|s| s.code().unwrap_or(-1))
    }

    /// pids of the daemon process(es) of this sandbox (not WebKit helpers, not the bus).
    pub fn daemon_pids(&self) -> Vec<u32> {
        let key = if self.is_legacy() {
            "app.py"
        } else {
            "llm-wrapperd"
        };
        proc::pids_in(self.runtime.to_str().unwrap())
            .into_iter()
            .filter(|p| proc::alive(*p))
            .filter(|p| {
                let c = proc::cmdline(*p);
                c.contains(key) && !c.contains("dbus-daemon")
            })
            .collect()
    }

    // ---- state file ----------------------------------------------------------------------------

    pub fn state(&self, instance: Option<&str>) -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(self.state_path(instance)).ok()?).ok()
    }

    pub fn wait_state(
        &self,
        instance: Option<&str>,
        what: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        self.wait_state_for(instance, what, Duration::from_secs(25), pred)
    }

    pub fn wait_state_for(
        &self,
        instance: Option<&str>,
        what: &str,
        limit: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        let start = Instant::now();
        loop {
            if let Some(s) = self.state(instance) {
                if pred(&s) {
                    return s;
                }
            }
            if start.elapsed() > limit {
                panic!(
                    "timed out after {limit:?} waiting for: {what}\nstate: {:?}\ndaemon log:\n{}",
                    self.state(instance),
                    self.daemon_log(0)
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn visible(&self, instance: Option<&str>) -> bool {
        self.state(instance)
            .and_then(|s| s["visible"].as_bool())
            .unwrap_or(false)
    }

    /// Start a hidden daemon on `provider_url` and wait until it reports it.
    pub fn start_hidden(&mut self, provider_url: &str) {
        self.start(&["--background", "--provider", provider_url]);
        self.wait_state(None, "daemon to report the provider", |s| {
            s["url"] == provider_url
        });
    }

    /// `--quit` through the launcher, then wait for the daemon to go away.
    pub fn quit(&mut self, instance: Option<&str>) {
        let mut args = vec!["--quit"];
        if let Some(i) = instance {
            args.extend(["--instance", i]);
        }
        let _ = self.run_os(
            args.iter().map(OsString::from).collect(),
            Duration::from_secs(15),
        );
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            if !self.socket_path(instance).exists() && !self.state_path(instance).exists() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn wait_gone(&self, instance: Option<&str>, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if !self.socket_path(instance).exists() && !self.state_path(instance).exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        false
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let rt = self.runtime.to_str().unwrap().to_owned();
        // polite first, then the safety net: nothing started inside this sandbox may survive it
        for inst in self
            .daemons
            .iter()
            .map(|(i, _, _)| i.clone())
            .collect::<std::collections::BTreeSet<_>>()
        {
            if self.socket_path(inst.as_deref()).exists() {
                let mut args: Vec<OsString> = vec!["--quit".into()];
                if let Some(i) = &inst {
                    args.extend(["--instance".into(), i.into()]);
                }
                let mut cmd = self.base_command(&[]);
                cmd.args(args).stdout(Stdio::null()).stderr(Stdio::null());
                if let Ok(mut c) = cmd.spawn() {
                    let start = Instant::now();
                    while c.try_wait().ok().flatten().is_none()
                        && start.elapsed() < Duration::from_secs(4)
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    let _ = c.kill();
                }
            }
        }
        thread::sleep(Duration::from_millis(150));
        for pid in proc::pids_in(&rt) {
            proc::kill9(pid);
        }
        for (_, c, _) in &mut self.daemons {
            let _ = c.kill();
            let _ = c.wait();
        }
        for c in &mut self.helpers {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(b) = &mut self.bus {
            let _ = b.kill();
            let _ = b.wait();
        }
        if !self.keep {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// Run `f`; on the legacy app it is *expected to fail* (a documented defect, see `docs/spec.md`),
/// and succeeding is itself an error so the list of known defects cannot rot. On any other
/// implementation it must simply pass.
pub fn known_defect(sb: &Sandbox, id: &str, why: &str, f: impl FnOnce()) {
    if sb.is_legacy() {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Err(_) => eprintln!("xfail {id}: {why}"),
            Ok(()) => panic!("{id} unexpectedly PASSES on the legacy app, so it is no longer a defect: update docs/spec.md and the test"),
        }
    } else {
        f();
    }
}
