//! Test support: a private runtime directory, a misbehaving mock daemon, and a way to run the
//! real `llm-wrapper` binary in a hermetic environment.
#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::{Request, Response};

pub const CLIENT: &str = env!("CARGO_BIN_EXE_llm-wrapper");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh `0700` directory under the system temp dir, removed on drop.
pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("lwc-{}-{n}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn socket(&self, instance: Option<&str>) -> PathBuf {
        match instance {
            Some(i) => self.0.join(format!("llm-wrapper-{i}.v1.sock")),
            None => self.0.join("llm-wrapper.v1.sock"),
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// ETXTBSY ("text file busy"): another test thread forked while this binary's freshly written copy
/// was still open for writing in this process. Retrying is the standard remedy and the condition
/// cannot arise outside the test suite.
fn spawn_retrying_etxtbsy(cmd: &mut Command) -> std::process::Child {
    for _ in 0..100 {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(26) => thread::sleep(Duration::from_millis(10)),
            other => return other.unwrap(),
        }
    }
    panic!("ETXTBSY persisted for 1 s");
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

/// Run the client with a scrubbed environment (only `XDG_RUNTIME_DIR` and a fixed `PATH`).
pub fn run_client(runtime: &Path, args: &[&str]) -> Out {
    run_binary(CLIENT, runtime, args, &[])
}

pub fn run_binary(
    bin: impl AsRef<Path>,
    runtime: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> Out {
    run_binary_os(
        bin,
        Some(runtime),
        args.iter().map(std::ffi::OsString::from).collect(),
        extra_env,
    )
}

pub fn run_binary_os(
    bin: impl AsRef<Path>,
    runtime: Option<&Path>,
    args: Vec<std::ffi::OsString>,
    extra_env: &[(&str, &str)],
) -> Out {
    let mut cmd = Command::new(bin.as_ref());
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null());
    if let Some(r) = runtime {
        cmd.env("XDG_RUNTIME_DIR", r);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_retrying_etxtbsy(&mut cmd);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let t_out = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let t_err = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    // never let a hung client hang the test run
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            panic!("client hung for 20 s");
        }
        thread::sleep(Duration::from_millis(2));
    };
    let elapsed = start.elapsed();
    Out {
        code: status.code().unwrap_or(-1),
        stdout: t_out.join().unwrap(),
        stderr: t_err.join().unwrap(),
        elapsed,
    }
}

#[derive(Clone)]
pub enum Behavior {
    /// Answer with this response.
    Reply(Response),
    /// Write exactly these bytes, then close.
    Raw(Vec<u8>),
    /// Read the request, then say nothing for this long.
    Silent(Duration),
    /// Close without reading.
    CloseImmediately,
    /// Send a valid reply one byte at a time.
    Trickle(Duration),
    /// Send a header and then data until the client goes away.
    Flood(Vec<u8>),
}

/// A daemon double speaking the real protocol (decoded with the production code).
pub struct MockDaemon {
    pub socket: PathBuf,
    requests: mpsc::Receiver<Result<Request, String>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    pub flood_sent: Arc<Mutex<usize>>,
}

impl MockDaemon {
    pub fn start(socket: PathBuf, behavior: Behavior) -> Self {
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flood_sent = Arc::new(Mutex::new(0_usize));
        let (stop2, flood2) = (stop.clone(), flood_sent.clone());
        let accept = thread::spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let (tx, behavior, flood) = (tx.clone(), behavior.clone(), flood2.clone());
                thread::spawn(move || serve(conn, &behavior, &tx, &flood));
            }
        });
        Self {
            socket,
            requests: rx,
            stop,
            accept: Some(accept),
            flood_sent,
        }
    }

    /// Next request seen (decoded by the production decoder), waiting up to 3 s.
    pub fn next_request(&self) -> Result<Request, String> {
        self.requests
            .recv_timeout(Duration::from_secs(3))
            .expect("daemon saw no request")
    }

    pub fn no_request_within(&self, d: Duration) -> bool {
        self.requests.recv_timeout(d).is_err()
    }
}

impl Drop for MockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

fn serve(
    mut conn: UnixStream,
    behavior: &Behavior,
    tx: &mpsc::Sender<Result<Request, String>>,
    flood: &Arc<Mutex<usize>>,
) {
    if matches!(behavior, Behavior::CloseImmediately) {
        return;
    }
    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
    let parsed = frame::read(&mut conn, Kind::Request)
        .map_err(|e| e.to_string())
        .and_then(|body| Request::decode(&body).map_err(|e| e.to_string()));
    let _ = tx.send(parsed);
    match behavior {
        Behavior::Reply(r) => {
            let _ = conn.write_all(&r.encode().unwrap());
        }
        Behavior::Raw(bytes) => {
            let _ = conn.write_all(bytes);
        }
        Behavior::Silent(d) => thread::sleep(*d),
        Behavior::Trickle(interval) => {
            for b in Response::ok().encode().unwrap() {
                if conn.write_all(&[b]).is_err() {
                    return;
                }
                thread::sleep(*interval);
            }
        }
        Behavior::Flood(header) => {
            let _ = conn.write_all(header);
            let chunk = [b'A'; 4096];
            while conn.write_all(&chunk).is_ok() {
                *flood.lock().unwrap() += chunk.len();
                if *flood.lock().unwrap() > 64 * 1024 * 1024 {
                    break;
                }
            }
        }
        Behavior::CloseImmediately => {}
    }
}
