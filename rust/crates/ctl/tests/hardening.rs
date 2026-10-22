//! Security refusals of the client. Each test builds the hostile situation on disk and checks that
//! the client refuses with exit code 4 *without* sending anything or executing anything.
//! Ownership tests need root (to `chown` to a foreign uid) and are skipped otherwise.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{run_binary, run_client, Behavior, MockDaemon, TestDir, CLIENT};
use llm_wrapper_ctl::daemon::DAEMON_NAME;
use llm_wrapper_ctl::runtime::current_uid;
use llm_wrapper_protocol::Response;

const FOREIGN_UID: u32 = 12_345;

fn chmod(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

fn is_root() -> bool {
    current_uid().unwrap() == 0
}

fn chown_foreign(p: &Path) {
    std::os::unix::fs::chown(p, Some(FOREIGN_UID), None).unwrap();
}

fn assert_refused(out: &common::Out, needle: &str) {
    assert_eq!(out.code, 4, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains(needle),
        "expected '{needle}' in: {}",
        out.stderr
    );
    assert!(out.stdout.is_empty());
}

// ---- runtime directory ------------------------------------------------------------------------

#[test]
fn runtime_dir_open_to_others_is_refused_without_connecting() {
    for mode in [0o755, 0o750, 0o770, 0o777, 0o701] {
        let dir = TestDir::new("open");
        let d = MockDaemon::start(dir.socket(None), Behavior::Reply(Response::ok()));
        chmod(dir.path(), mode);
        let out = run_client(dir.path(), &["--show"]);
        assert_refused(&out, "0700");
        assert!(
            d.no_request_within(Duration::from_millis(100)),
            "connected despite mode {mode:o}"
        );
        chmod(dir.path(), 0o700); // so TestDir can clean up
    }
}

#[test]
fn runtime_dir_that_is_a_symlink_is_refused() {
    let real = TestDir::new("real");
    let holder = TestDir::new("holder");
    let link = holder.path().join("rt");
    symlink(real.path(), &link).unwrap();
    assert_refused(&run_client(&link, &["--show"]), "symbolic link");
}

#[test]
fn runtime_dir_must_exist_be_a_directory_and_be_absolute() {
    let holder = TestDir::new("shape");
    assert_refused(
        &run_client(&holder.path().join("missing"), &["--show"]),
        "does not exist",
    );
    let file = holder.path().join("file");
    fs::write(&file, b"x").unwrap();
    assert_refused(&run_client(&file, &["--show"]), "not a directory");
    assert_refused(
        &run_client(Path::new("relative/dir"), &["--show"]),
        "absolute",
    );
}

#[test]
fn runtime_dir_owned_by_someone_else_is_refused() {
    if !is_root() {
        eprintln!("SKIP: needs root to chown");
        return;
    }
    let dir = TestDir::new("foreign");
    chown_foreign(dir.path());
    assert_refused(&run_client(dir.path(), &["--show"]), "owned by uid 12345");
    std::os::unix::fs::chown(dir.path(), Some(0), None).unwrap();
}

#[test]
fn unset_or_empty_xdg_falls_back_to_run_user_uid_and_is_still_verified() {
    for xdg in [None, Some(Path::new(""))] {
        let out = common::run_binary_os(CLIENT, xdg, vec!["--show".into()], &[]);
        // either /run/user/UID is absent/unsafe (4) or it is fine and there is simply no daemon (3)
        assert!(
            matches!(out.code, 3 | 4),
            "{xdg:?}: code {} / {}",
            out.code,
            out.stderr
        );
        assert!(
            out.stderr.contains("/run/user/") || out.code == 3,
            "{}",
            out.stderr
        );
    }
}

#[test]
fn over_long_socket_path_is_refused() {
    let top = TestDir::new("long");
    let mut p: PathBuf = top.path().to_owned();
    while p.as_os_str().len() < 110 {
        p.push("d".repeat(40));
        fs::create_dir(&p).unwrap();
        chmod(&p, 0o700);
    }
    assert_refused(&run_client(&p, &["--show"]), "too long");
}

// ---- the socket itself --------------------------------------------------------------------------

fn bind(dir: &TestDir) -> UnixListener {
    UnixListener::bind(dir.socket(None)).unwrap()
}

#[test]
fn socket_open_to_others_is_refused() {
    let dir = TestDir::new("sockmode");
    let _l = bind(&dir);
    chmod(&dir.socket(None), 0o666);
    assert_refused(&run_client(dir.path(), &["--show"]), "0600");
}

#[test]
fn a_symlink_or_regular_file_in_place_of_the_socket_is_refused() {
    let dir = TestDir::new("socktype");
    let target = dir.path().join("elsewhere.sock");
    let _l = UnixListener::bind(&target).unwrap();
    chmod(&target, 0o600);
    symlink(&target, dir.socket(None)).unwrap();
    assert_refused(&run_client(dir.path(), &["--show"]), "not a socket");
    fs::remove_file(dir.socket(None)).unwrap();
    fs::write(dir.socket(None), b"not a socket").unwrap();
    assert_refused(&run_client(dir.path(), &["--show"]), "not a socket");
}

#[test]
fn a_socket_owned_by_someone_else_is_refused() {
    if !is_root() {
        eprintln!("SKIP: needs root to chown");
        return;
    }
    let dir = TestDir::new("sockowner");
    let _l = bind(&dir);
    chmod(&dir.socket(None), 0o600);
    chown_foreign(&dir.socket(None));
    assert_refused(&run_client(dir.path(), &["--show"]), "owned by uid 12345");
}

// ---- starting the daemon ------------------------------------------------------------------------

/// `bin/` holding a copy of the client and (optionally) a stub daemon that records how it was run.
struct Install {
    root: TestDir,
    bin: PathBuf,
    out: PathBuf,
}

impl Install {
    fn new(label: &str, stub: bool) -> Self {
        let root = TestDir::new(label);
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        chmod(&bin, 0o700);
        fs::copy(CLIENT, bin.join("llm-wrapper")).unwrap();
        chmod(&bin.join("llm-wrapper"), 0o700);
        let out = root.path().join("stub.out");
        let me = Self { root, bin, out };
        if stub {
            me.write_stub(0o700);
        }
        me
    }

    fn stub_path(&self) -> PathBuf {
        self.bin.join(DAEMON_NAME)
    }

    fn write_stub(&self, mode: u32) {
        let script = "#!/bin/sh\n{ echo \"argv0=$0\"; for a in \"$@\"; do echo \"arg=$a\"; done; } > \"$LW_STUB_OUT\"\n";
        fs::write(self.stub_path(), script).unwrap();
        chmod(&self.stub_path(), mode);
    }

    fn client(&self) -> PathBuf {
        self.bin.join("llm-wrapper")
    }

    fn run(&self, args: &[&str]) -> common::Out {
        let rt = TestDir::new("rt");
        run_binary(
            self.client(),
            rt.path(),
            args,
            &[("LW_STUB_OUT", self.out.to_str().unwrap())],
        )
    }

    fn stub_saw(&self) -> Option<String> {
        fs::read_to_string(&self.out).ok()
    }
}

#[test]
fn starts_the_sibling_daemon_with_the_original_arguments_verbatim() {
    let inst = Install::new("exec", true);
    let args = [
        "--background",
        "--provider",
        "gemini",
        "--user-agent",
        "Mozilla/5.0 (X11; Linux) x",
        "--instance=w",
    ];
    let out = inst.run(&args);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let saw = inst.stub_saw().expect("stub did not run");
    // for a shell script `$0` is the script path: this proves *which file* was executed
    let want = format!("argv0={}\n", inst.stub_path().display())
        + "arg=--background\narg=--provider\narg=gemini\narg=--user-agent\narg=Mozilla/5.0 (X11; Linux) x\narg=--instance=w\n";
    assert_eq!(saw, want);
}

#[test]
fn an_invalid_command_line_never_starts_a_daemon() {
    let inst = Install::new("noexec", true);
    assert_eq!(inst.run(&["--bogus"]).code, 2);
    assert!(inst.stub_saw().is_none());
}

#[test]
fn quit_with_no_daemon_does_not_start_one() {
    let inst = Install::new("quitnoexec", true);
    assert_eq!(inst.run(&["--quit"]).code, 0);
    assert!(inst.stub_saw().is_none());
}

#[test]
fn the_daemon_is_never_taken_from_path() {
    let inst = Install::new("path", false); // no sibling
    let decoy_dir = TestDir::new("decoy");
    let decoy_out = decoy_dir.path().join("decoy.out");
    fs::write(
        decoy_dir.path().join(DAEMON_NAME),
        format!("#!/bin/sh\necho pwned > {}\n", decoy_out.display()),
    )
    .unwrap();
    chmod(&decoy_dir.path().join(DAEMON_NAME), 0o755);
    let rt = TestDir::new("rt");
    let path = format!("{}:/usr/bin:/bin", decoy_dir.path().display());
    let out = run_binary(inst.client(), rt.path(), &["--show"], &[("PATH", &path)]);
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        out.stderr.contains("not installed next to llm-wrapper"),
        "{}",
        out.stderr
    );
    assert!(
        !decoy_out.exists(),
        "a daemon found through PATH was executed"
    );
}

#[test]
fn untrustworthy_daemons_are_refused_and_not_executed() {
    // symlink
    let inst = Install::new("d-symlink", false);
    let real = inst.root.path().join("real-daemon");
    fs::write(&real, "#!/bin/sh\necho x > \"$LW_STUB_OUT\"\n").unwrap();
    chmod(&real, 0o700);
    symlink(&real, inst.stub_path()).unwrap();
    assert_refused(&inst.run(&["--show"]), "symbolic link");
    assert!(inst.stub_saw().is_none());

    // group- and world-writable file
    for mode in [0o770, 0o707, 0o777, 0o720] {
        let inst = Install::new("d-writable", true);
        chmod(&inst.stub_path(), mode);
        assert_refused(&inst.run(&["--show"]), "writable by other users");
        assert!(inst.stub_saw().is_none(), "executed at mode {mode:o}");
    }

    // not executable
    let inst = Install::new("d-noexec", true);
    chmod(&inst.stub_path(), 0o600);
    assert_refused(&inst.run(&["--show"]), "not executable");

    // a directory named like the daemon
    let inst = Install::new("d-dir", false);
    fs::create_dir(inst.stub_path()).unwrap();
    assert_refused(&inst.run(&["--show"]), "not a regular file");
}

#[test]
fn a_daemon_owned_by_someone_else_is_refused() {
    if !is_root() {
        eprintln!("SKIP: needs root to chown");
        return;
    }
    let inst = Install::new("d-owner", true);
    chown_foreign(&inst.stub_path());
    assert_refused(&inst.run(&["--show"]), "owned by another user");
    assert!(inst.stub_saw().is_none());
}

#[test]
fn an_install_directory_writable_by_others_is_refused() {
    let inst = Install::new("d-dirmode", true);
    for mode in [0o770, 0o777, 0o702] {
        chmod(&inst.bin, mode);
        assert_refused(&inst.run(&["--show"]), "writable by other users");
        assert!(inst.stub_saw().is_none());
    }
    chmod(&inst.bin, 0o700);
}

#[test]
fn a_world_writable_ancestor_is_refused_unless_it_is_sticky() {
    let top = TestDir::new("ancestor");
    let shared = top.path().join("shared");
    fs::create_dir(&shared).unwrap();
    let bin = shared.join("bin");
    fs::create_dir(&bin).unwrap();
    chmod(&bin, 0o700);
    fs::copy(CLIENT, bin.join("llm-wrapper")).unwrap();
    chmod(&bin.join("llm-wrapper"), 0o700);
    let stub = bin.join(DAEMON_NAME);
    let out_file = top.path().join("out");
    fs::write(&stub, "#!/bin/sh\necho ran > \"$LW_STUB_OUT\"\n").unwrap();
    chmod(&stub, 0o700);
    let rt = TestDir::new("rt");
    let env = [("LW_STUB_OUT", out_file.to_str().unwrap())];

    chmod(&shared, 0o777); // anyone could rename `bin` away and put their own in its place
    let out = run_binary(bin.join("llm-wrapper"), rt.path(), &["--show"], &env);
    assert_refused(&out, "writable by other users");
    assert!(!out_file.exists());

    chmod(&shared, 0o1777); // like /tmp: sticky, entries cannot be renamed by others
    let out = run_binary(bin.join("llm-wrapper"), rt.path(), &["--show"], &env);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out_file.exists());
    chmod(&shared, 0o700);
}
