//! A stand-in for a web chat site, so the suite never needs the network or an account.
//!
//! It serves a small page (with `#prompt-textarea` like ChatGPT) for every path and records every
//! request, which is how tests observe what the wrapper loaded and with which User-Agent.
//! Responses are `no-store`, so a reload always reaches the server.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One recorded request.
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub user_agent: String,
    pub at: Instant,
}

pub struct FakeChat {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

const PAGE: &str = "<!doctype html><title>fake chat</title><body style=\"margin:0\">\
<a href=\"/c/abc-123\" style=\"display:block;width:700px;height:200px;background:#cde\">open conversation</a>\
<textarea id=\"prompt-textarea\" rows=3 cols=60></textarea></body>";

impl FakeChat {
    pub fn start() -> Self {
        Self::bind("127.0.0.1:0".parse().unwrap(), Arc::default())
    }

    fn bind(addr: SocketAddr, hits: Arc<Mutex<Vec<Hit>>>) -> Self {
        let listener = TcpListener::bind(addr).expect("bind fake chat");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (h, s) = (hits.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        let h = h.clone();
                        thread::spawn(move || serve(conn, &h));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(4)),
                }
            }
        });
        Self {
            addr,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `http://127.0.0.1:PORT/`
    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    pub fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    /// Requests for exactly `path`.
    pub fn count(&self, path: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.path == path)
            .count()
    }

    pub fn total(&self) -> usize {
        self.hits.lock().unwrap().len()
    }

    pub fn last(&self) -> Option<Hit> {
        self.hits.lock().unwrap().last().cloned()
    }

    /// Wait until `cond` holds for the recorded hits.
    pub fn wait(&self, timeout: Duration, cond: impl Fn(&[Hit]) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if cond(&self.hits.lock().unwrap()) {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// Simulate the network going away: stop answering and close the port.
    pub fn go_offline(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Come back on the same port, keeping the recorded history.
    pub fn go_online(&mut self) {
        assert!(self.thread.is_none(), "already online");
        let hits = self.hits.clone();
        *self = Self::bind(self.addr, hits);
    }
}

impl Drop for FakeChat {
    fn drop(&mut self) {
        self.go_offline();
    }
}

fn serve(mut conn: TcpStream, hits: &Mutex<Vec<Hit>>) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        match conn.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.lines();
    let path = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let user_agent = lines
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                .map(|(_, v)| v.trim().to_owned())
        })
        .unwrap_or_default();
    if path == "/favicon.ico" {
        let _ = conn
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    hits.lock().unwrap().push(Hit {
        path,
        user_agent,
        at: Instant::now(),
    });
    let body = PAGE.as_bytes();
    let _ = write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(body);
}
