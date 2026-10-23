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

impl Hit {
    /// The query string, percent-decoded (what `encodeURIComponent` in the pages produced).
    pub fn query(&self) -> String {
        let raw = self.path.split_once('?').map_or("", |(_, q)| q);
        let bytes = raw.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                if let Some(v) = raw
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// The query string as JSON, for the pages that report an object.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.query()).unwrap_or(serde_json::Value::Null)
    }
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
    let route = path.split('?').next().unwrap_or("/").to_owned();
    hits.lock().unwrap().push(Hit {
        path,
        user_agent,
        at: Instant::now(),
    });
    let (content_type, extra, body) = match route.as_str() {
        "/probe" => ("text/html; charset=utf-8", "", PROBE),
        "/probe-frame" => ("text/html; charset=utf-8", "", PROBE_FRAME),
        "/open" => ("text/html; charset=utf-8", "", OPEN),
        "/open-many" => ("text/html; charset=utf-8", "", OPEN_MANY),
        "/download-many" => ("text/html; charset=utf-8", "", DOWNLOAD_MANY),
        "/perm" => ("text/html; charset=utf-8", "", PERMISSIONS),
        "/perm-click" => ("text/html; charset=utf-8", "", PERMISSION_CLICK),
        "/link" => ("text/html; charset=utf-8", "", LINK),
        "/blank" => ("text/html; charset=utf-8", "", BLANK),
        "/upload" => ("text/html; charset=utf-8", "", UPLOAD),
        "/download" => (
            "application/octet-stream",
            "Content-Disposition: attachment; filename=\"../../evil.bin\"\r\n",
            DOWNLOAD,
        ),
        _ => ("text/html; charset=utf-8", "", PAGE),
    };
    let body = body.as_bytes();
    let _ = write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(body);
}

// ---- pages for the Phase 3 behaviour (navigation, windows, permissions, transfers) --------------
//
// Each reports what it saw by requesting a path with the facts in the query string; tests read the
// recorded hits. `u` in the query is the address the page links or opens.

/// What a web page can tell about the browser it runs in, and whether the app's style sheet applied.
const PROBE: &str = "<!doctype html><title>probe</title><body>\
<pre><code id=c>x</code></pre><iframe src=\"/probe-frame\"></iframe>\
<script>(async () => {\
 const r = {webdriver: navigator.webdriver, vendor: navigator.vendor, chrome: typeof window.chrome,\
  runtime: !!(window.chrome && window.chrome.runtime), plugins: navigator.plugins.length,\
  ua: navigator.userAgent, contain: getComputedStyle(document.getElementById('c')).contain};\
 const d = navigator.userAgentData;\
 if (d) { r.brands = d.brands.map(b => b.brand + '/' + b.version).join(','); r.mobile = d.mobile;\
  r.platform = d.platform; const h = await d.getHighEntropyValues([]);\
  r.arch = h.architecture; r.bitness = h.bitness; r.full = h.fullVersionList.map(b => b.version).join(','); }\
 await fetch('/report?' + encodeURIComponent(JSON.stringify(r)));\
})();</script>";

/// The same, from inside a frame (the bot checks run in frames).
const PROBE_FRAME: &str = "<!doctype html><title>frame</title><script>\
fetch('/report-frame?' + encodeURIComponent(JSON.stringify({webdriver: navigator.webdriver,\
 vendor: navigator.vendor, brands: navigator.userAgentData && navigator.userAgentData.brands.length})));\
</script>";

/// Opens `u` in a new window at once, with no click (what a sign-in flow or an ad does).
const OPEN: &str = "<!doctype html><title>open</title><script>\
window.open(new URLSearchParams(location.search).get('u'));\
fetch('/opener-ran');</script>";

/// Opens six windows of the same site.
const OPEN_MANY: &str = "<!doctype html><title>many</title><script>\
for (let i = 0; i < 6; i++) window.open('/popup-page?n=' + i);\
fetch('/opener-ran');</script>";

/// Asks for every permission a page can ask for. It reports each answer as soon as it has it
/// (`/perm-step?name=answer`), then everything together (`/report-perm?{...}`), so a request that is
/// never answered shows up as the step that is missing.
const PERMISSIONS: &str = "<!doctype html><title>permissions</title><script>(async () => {\
 const r = {};\
 const step = async (name, value) => { r[name] = value;\
  await fetch('/perm-step?' + encodeURIComponent(name + '=' + value)); };\
 const within = (p) => Promise.race([p, new Promise(res => setTimeout(() => res('timeout'), 4000))]);\
 await step('getUserMedia', navigator.mediaDevices ? typeof navigator.mediaDevices.getUserMedia : 'no mediaDevices');\
 await step('notification', await within(Notification.requestPermission()));\
 await step('clipboard', (navigator.clipboard && navigator.clipboard.readText)\
  ? await within(navigator.clipboard.readText().then(() => 'read', e => 'denied:' + e.name)) : 'no clipboard api');\
 await step('geolocation', await within(new Promise(res => navigator.geolocation.getCurrentPosition(\
  () => res('granted'), e => res('error:' + e.code)))));\
 await fetch('/report-perm?' + encodeURIComponent(JSON.stringify(r)));\
})();</script>";

/// A big button that asks for the notification permission when clicked (a request with a gesture).
const PERMISSION_CLICK: &str = "<!doctype html><title>ask</title><body style=\"margin:0\">\
<button id=b style=\"display:block;width:700px;height:200px\">ask</button>\
<script>document.getElementById('b').onclick = async () => {\
 const answer = await Notification.requestPermission();\
 await fetch('/report-click?' + encodeURIComponent(answer)); };</script>";

/// A big link to `u` in the same window (to be clicked).
const LINK: &str = "<!doctype html><title>link</title><body style=\"margin:0\">\
<a id=l style=\"display:block;width:700px;height:200px;background:#cde\">link</a>\
<script>document.getElementById('l').href = new URLSearchParams(location.search).get('u');</script>";

/// A big link to `u` for a new window (`target=_blank`, to be clicked).
const BLANK: &str = "<!doctype html><title>blank</title><body style=\"margin:0\">\
<a id=l target=_blank style=\"display:block;width:700px;height:200px;background:#cde\">link</a>\
<script>document.getElementById('l').href = new URLSearchParams(location.search).get('u');</script>";

/// A big file input (to be clicked); reports the name of the file it was given.
const UPLOAD: &str = "<!doctype html><title>upload</title><body style=\"margin:0\">\
<input id=f type=file style=\"display:block;width:700px;height:200px\">\
<script>document.getElementById('f').onchange = e =>\
 fetch('/uploaded?' + encodeURIComponent(e.target.files[0].name + ':' + e.target.files[0].size));</script>";

/// Starts five downloads at once, as a page that will not take no for an answer would.
const DOWNLOAD_MANY: &str = "<!doctype html><title>downloads</title><body><script>\
for (let i = 0; i < 5; i++) { const f = document.createElement('iframe'); f.style.display = 'none';\
 f.src = '/download?n=' + i; document.body.appendChild(f); }</script></body>";

/// What a download carries.
const DOWNLOAD: &str = "payload-bytes";
