//! The control socket, served from the `GLib` main loop — no threads.
//!
//! Every connection is its own future with a hard deadline, so a client that connects and says
//! nothing, or sends one byte a second, costs the daemon a timer and nothing else: it cannot delay
//! anybody (the legacy daemon served clients one at a time and an idle one blocked the rest for a
//! second). The peer's uid is checked with `SO_PEERCRED`, requests are bounded and re-validated, and
//! the answer is written only *after* the command has been carried out, so it is the truth.

use std::cell::Cell;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixListener;
use std::rc::Rc;
use std::time::Duration;

use gtk4::gio::{self, prelude::*};
use gtk4::glib;
use llm_wrapper_platform::runtime;
use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::limits::HEADER_LEN;
use llm_wrapper_protocol::{ProtocolError, Request, Response, Status};

use crate::log;

/// Time a client has to send a complete request, and (separately) to take the answer.
pub const DEADLINE: Duration = Duration::from_secs(1);
/// Connections served at once; beyond this the newcomer is told "busy" and dropped.
pub const MAX_IN_FLIGHT: usize = 32;

/// What to do about a request: the answer, and whether the daemon should exit once it is sent.
pub struct Reply {
    pub response: Response,
    pub then_quit: bool,
}

/// Executes a decoded, validated request.
pub type Handler = Rc<dyn Fn(Request) -> Reply>;

/// A running listener. Dropping it stops accepting.
pub struct Server {
    service: gio::SocketService,
}

impl Server {
    /// Stop accepting connections.
    pub fn stop(&self) {
        self.service.stop();
        self.service.close();
    }
}

/// Start serving `listener` on the thread-default main context. `on_quit` runs after the reply to
/// a `then_quit` request has been written.
pub fn start(
    listener: UnixListener,
    handler: Handler,
    on_quit: Rc<dyn Fn()>,
) -> Result<Server, String> {
    let socket = gio::Socket::from_fd(OwnedFd::from(listener)).map_err(|e| e.to_string())?;
    let service = gio::SocketService::new();
    service
        .add_socket(&socket, None::<&glib::Object>)
        .map_err(|e| e.to_string())?;
    let own_uid = runtime::current_uid().map_err(|e| e.to_string())?;
    let in_flight = Rc::new(Cell::new(0_usize));
    service.connect_incoming(move |_, connection, _| {
        glib::spawn_future_local(serve(
            connection.clone(),
            handler.clone(),
            on_quit.clone(),
            in_flight.clone(),
            own_uid,
        ));
        true
    });
    service.start();
    Ok(Server { service })
}

struct Slot(Rc<Cell<usize>>);

impl Slot {
    fn take(counter: &Rc<Cell<usize>>) -> Self {
        counter.set(counter.get() + 1);
        Self(counter.clone())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

/// The peer may talk to us only if the kernel (`SO_PEERCRED`) says it runs as our uid. Anything
/// else — another user, or credentials that cannot be read — is refused.
///
/// This compares uids itself instead of calling `Credentials::is_same_user`: for a *different* user
/// `GLib` returns FALSE without setting an error, which the gio-rs binding reports as `Ok(())` in a
/// release build (the check would pass for everybody) and as a panic in a debug build.
fn is_us(peer_uid: Result<u32, glib::Error>, own_uid: u32) -> bool {
    peer_uid.is_ok_and(|uid| uid == own_uid)
}

fn same_user(connection: &gio::SocketConnection, own_uid: u32) -> bool {
    is_us(
        connection
            .socket()
            .credentials()
            .and_then(|peer| peer.unix_user()),
        own_uid,
    )
}

/// Why no request could be read, and what (if anything) to tell the client.
enum Early {
    Silent,
    Answer(Response),
}

async fn read_exact(input: &gio::InputStream, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    match input
        .read_all_future(vec![0_u8; len], glib::Priority::DEFAULT)
        .await
    {
        Ok((buf, read, None)) if read == len => Some(buf),
        _ => None,
    }
}

async fn read_request(connection: &gio::SocketConnection) -> Result<Request, Early> {
    let input = connection.input_stream();
    let header = read_exact(&input, HEADER_LEN).await.ok_or(Early::Silent)?;
    let header: [u8; HEADER_LEN] = header.try_into().map_err(|_| Early::Silent)?;
    let len = match frame::parse_header(&header, Kind::Request) {
        Ok(len) => len,
        // a peer that does not speak this protocol at all gets no reply
        Err(ProtocolError::BadMagic | ProtocolError::UnexpectedKind(_)) => {
            return Err(Early::Silent)
        }
        Err(ProtocolError::UnsupportedVersion(v)) => {
            let msg = format!("unsupported protocol version {v}; this daemon speaks version 1");
            return Err(Early::Answer(Response::with_message(
                Status::Unsupported,
                &msg,
            )));
        }
        Err(e) => {
            return Err(Early::Answer(Response::with_message(
                Status::Rejected,
                &format!("invalid request: {e}"),
            )))
        }
    };
    let body = read_exact(&input, len).await.ok_or(Early::Silent)?;
    Request::decode(&body).map_err(|e| {
        Early::Answer(Response::with_message(
            Status::Rejected,
            &format!("invalid request: {e}"),
        ))
    })
}

async fn send(connection: &gio::SocketConnection, response: &Response) {
    let Ok(bytes) = response.encode() else { return };
    let write = async {
        let _ = connection
            .output_stream()
            .write_all_future(bytes, glib::Priority::DEFAULT)
            .await;
    };
    let _ = glib::future_with_timeout(DEADLINE, Box::pin(write)).await;
}

async fn serve(
    connection: gio::SocketConnection,
    handler: Handler,
    on_quit: Rc<dyn Fn()>,
    in_flight: Rc<Cell<usize>>,
    own_uid: u32,
) {
    let _slot = Slot::take(&in_flight);
    if in_flight.get() > MAX_IN_FLIGHT {
        send(
            &connection,
            &Response::with_message(Status::Busy, "too many clients at once; try again"),
        )
        .await;
        return;
    }
    if !same_user(&connection, own_uid) {
        log::warn("refused a connection from another user");
        return;
    }
    let reply = match glib::future_with_timeout(DEADLINE, Box::pin(read_request(&connection))).await
    {
        Err(_) => {
            log::warn("a client did not finish its request in time; dropped");
            return;
        }
        Ok(Err(Early::Silent)) => return,
        Ok(Err(Early::Answer(response))) => Reply {
            response,
            then_quit: false,
        },
        Ok(Ok(request)) => handler(request),
    };
    send(&connection, &reply.response).await;
    let _ =
        glib::future_with_timeout(DEADLINE, connection.close_future(glib::Priority::DEFAULT)).await;
    if reply.then_quit {
        on_quit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Instant;

    use llm_wrapper_platform::client;
    use llm_wrapper_protocol::Flags;

    struct Rig {
        socket: PathBuf,
        ctx: glib::MainContext,
        _server: Server,
        seen: Rc<RefCell<Vec<Request>>>,
        quit: Rc<Cell<bool>>,
    }

    /// A server whose handler records requests, quits on `--quit`, and refuses `--hide`.
    fn rig(label: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("lwd-ipc-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let ctx = glib::MainContext::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let quit = Rc::new(Cell::new(false));
        let (s2, q2, q3) = (seen.clone(), quit.clone(), quit.clone());
        let handler: Handler = Rc::new(move |req: Request| {
            s2.borrow_mut().push(req.clone());
            if req.flags.contains(Flags::QUIT) {
                return Reply {
                    response: Response::ok(),
                    then_quit: true,
                };
            }
            if req.flags.contains(Flags::HIDE) {
                return Reply {
                    response: Response::with_message(Status::Rejected, "no hiding today"),
                    then_quit: false,
                };
            }
            Reply {
                response: Response::ok(),
                then_quit: false,
            }
        });
        let server = ctx
            .with_thread_default(|| {
                start(listener, handler, Rc::new(move || q3.set(true))).unwrap()
            })
            .unwrap();
        let _ = q2;
        Rig {
            socket,
            ctx,
            _server: server,
            seen,
            quit,
        }
    }

    impl Rig {
        /// Run `client` on a thread while this thread serves the main context.
        fn run<T: Send + 'static>(&self, client: impl FnOnce() -> T + Send + 'static) -> T {
            let handle = thread::spawn(client);
            self.ctx
                .with_thread_default(|| {
                    while !handle.is_finished() {
                        self.ctx.iteration(false);
                        thread::sleep(Duration::from_millis(1));
                    }
                })
                .unwrap();
            handle.join().unwrap()
        }

        fn ask(&self, flags: Flags) -> Response {
            let socket = self.socket.clone();
            self.run(move || {
                let stream = client::connect(&socket, Duration::from_secs(2)).unwrap();
                client::exchange(
                    &stream,
                    &Request {
                        flags,
                        ..Request::default()
                    },
                    Duration::from_secs(3),
                )
                .unwrap()
            })
        }
    }

    fn raw(socket: &PathBuf, bytes: &[u8]) -> Vec<u8> {
        let mut s = UnixStream::connect(socket).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(bytes).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        out
    }

    #[test]
    fn a_valid_request_is_executed_and_answered_after_the_fact() {
        let r = rig("valid");
        assert_eq!(r.ask(Flags::SHOW), Response::ok());
        assert_eq!(r.seen.borrow().len(), 1);
        assert!(r.seen.borrow()[0].flags.contains(Flags::SHOW));
        // the handler's refusal reaches the client unchanged
        let refused = r.ask(Flags::HIDE);
        assert_eq!(
            (refused.status, refused.message.as_str()),
            (Status::Rejected, "no hiding today")
        );
    }

    #[test]
    fn quit_is_answered_first_and_only_then_carried_out() {
        let r = rig("quit");
        assert!(!r.quit.get());
        assert_eq!(
            r.ask(Flags::QUIT),
            Response::ok(),
            "the client must get its answer"
        );
        r.ctx
            .with_thread_default(|| {
                for _ in 0..20 {
                    r.ctx.iteration(false);
                }
            })
            .unwrap();
        assert!(r.quit.get());
    }

    #[test]
    fn garbage_gets_silence_and_the_daemon_carries_on() {
        let r = rig("garbage");
        for bytes in [
            &b"--toggle\n"[..],
            b"OK\n",
            b"LX\x01\x01\x00\x00",
            b"LW\x01\x02\x00\x00",
            &[0xff; 64][..],
            b"",
        ] {
            let socket = r.socket.clone();
            let sent = bytes.to_vec();
            let for_thread = sent.clone();
            let out = r.run(move || raw(&socket, &for_thread));
            assert!(out.is_empty(), "replied to garbage {sent:?}: {out:?}");
        }
        assert!(r.seen.borrow().is_empty(), "garbage reached the handler");
        assert_eq!(r.ask(Flags::SHOW), Response::ok());
    }

    #[test]
    fn a_different_version_and_malformed_bodies_get_a_clear_answer() {
        let r = rig("answers");
        let socket = r.socket.clone();
        let out = r.run(move || raw(&socket, b"LW\x09\x01\x00\x00"));
        let resp = Response::decode(&out[6..]).unwrap();
        assert_eq!(resp.status, Status::Unsupported);
        assert!(resp.message.contains("version 9"), "{}", resp.message);

        // 60000-byte body declared: refused before reading it
        let socket = r.socket.clone();
        let out = r.run(move || raw(&socket, b"LW\x01\x01\xea\x60"));
        let resp = Response::decode(&out[6..]).unwrap();
        assert_eq!(resp.status, Status::Rejected);

        // a well-framed body with an undefined flag bit
        let socket = r.socket.clone();
        let out = r.run(move || raw(&socket, b"LW\x01\x01\x00\x05\x01\x00\x02\x01\x00"));
        let resp = Response::decode(&out[6..]).unwrap();
        assert_eq!(resp.status, Status::Rejected);
        assert!(resp.message.contains("unknown flag"), "{}", resp.message);
        assert!(r.seen.borrow().is_empty());
    }

    #[test]
    fn an_idle_client_delays_nobody_and_is_cut_off_at_the_deadline() {
        let r = rig("idle");
        let idle = UnixStream::connect(&r.socket).unwrap();
        idle.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        assert_eq!(r.ask(Flags::SHOW), Response::ok());
        assert!(
            start.elapsed() < Duration::from_millis(300),
            "queued behind an idle client: {:?}",
            start.elapsed()
        );
        // the idle one is dropped when its time is up
        let t = Instant::now();
        let h = thread::spawn(move || {
            let mut idle = idle;
            let mut buf = [0_u8; 8];
            idle.read(&mut buf)
        });
        r.run(|| thread::sleep(Duration::from_millis(1400)));
        assert_eq!(h.join().unwrap().unwrap(), 0, "expected end-of-file");
        assert!(t.elapsed() < Duration::from_millis(2500));
    }

    #[test]
    fn a_client_that_trickles_is_cut_off_without_reaching_the_handler() {
        let r = rig("trickle");
        let socket = r.socket.clone();
        let started = r.run(move || {
            let mut s = UnixStream::connect(&socket).unwrap();
            let t = Instant::now();
            for b in b"LW\x01\x01\x00\x05\x01\x00\x02\x00\x00" {
                if s.write_all(&[*b]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
            t.elapsed()
        });
        assert!(started < Duration::from_secs(3));
        assert!(
            r.seen.borrow().is_empty(),
            "a request that took longer than the deadline was executed"
        );
    }

    #[test]
    fn too_many_simultaneous_clients_are_told_busy_and_the_daemon_recovers() {
        let r = rig("flood");
        let socket = r.socket.clone();
        let (busy, ok_after) = r.run(move || {
            let held: Vec<UnixStream> = (0..MAX_IN_FLIGHT + 8)
                .map(|_| UnixStream::connect(&socket).unwrap())
                .collect();
            thread::sleep(Duration::from_millis(300));
            let mut busy = 0;
            for mut s in held {
                s.set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut buf = [0_u8; 64];
                if let Ok(n) = s.read(&mut buf) {
                    if n > 6 && Response::decode(&buf[6..n]).is_ok_and(|r| r.status == Status::Busy)
                    {
                        busy += 1;
                    }
                }
            }
            thread::sleep(Duration::from_millis(1200)); // the idle ones time out
            let stream = client::connect(&socket, Duration::from_secs(2)).unwrap();
            let ok =
                client::exchange(&stream, &Request::default(), Duration::from_secs(3)).unwrap();
            (busy, ok)
        });
        assert!(busy >= 8, "only {busy} clients were told 'busy'");
        assert_eq!(ok_after, Response::ok());
    }

    #[test]
    fn many_well_behaved_clients_at_once_all_get_answers() {
        let r = rig("many");
        let socket = r.socket.clone();
        let ok = r.run(move || {
            let hs: Vec<_> = (0..24)
                .map(|_| {
                    let socket = socket.clone();
                    thread::spawn(move || {
                        let s = client::connect(&socket, Duration::from_secs(2)).unwrap();
                        client::exchange(
                            &s,
                            &Request {
                                flags: Flags::SHOW,
                                ..Request::default()
                            },
                            Duration::from_secs(3),
                        )
                        .unwrap()
                    })
                })
                .collect();
            hs.into_iter()
                .map(|h| h.join().unwrap())
                .filter(|r| *r == Response::ok())
                .count()
        });
        assert_eq!(ok, 24);
        assert_eq!(r.seen.borrow().len(), 24);
    }

    /// The "other user" half of `a_connection_from_another_user_is_refused`: the test binary runs
    /// this as a different uid, connects, sends a valid request and prints how many bytes came back.
    #[test]
    fn other_users_client() {
        let Some(path) = std::env::var_os("LW_TEST_PEER_SOCKET") else {
            return;
        };
        let request = Request {
            flags: Flags::SHOW,
            ..Request::default()
        }
        .encode()
        .unwrap();
        let answered = raw(&PathBuf::from(path), &request);
        println!("PEER-REPLY-BYTES={}", answered.len());
    }

    #[test]
    fn only_our_own_uid_is_accepted() {
        let unreadable = || glib::Error::new(gio::IOErrorEnum::Failed, "no credentials");
        assert!(is_us(Ok(1000), 1000));
        assert!(!is_us(Ok(1001), 1000));
        assert!(!is_us(Ok(0), 1000));
        assert!(!is_us(Err(unreadable()), 1000));
    }

    static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// `GLib` catches a panic inside a task of the main loop, so it looks like a dropped connection;
    /// count panics to tell "refused by the check" from "the check blew up".
    fn count_panics() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                PANICS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                previous(info);
            }));
        });
    }

    #[test]
    fn a_connection_from_another_user_is_refused() {
        use std::os::unix::process::CommandExt;
        count_panics();
        let panics_before = PANICS.load(std::sync::atomic::Ordering::SeqCst);
        if runtime::current_uid().unwrap() != 0 {
            eprintln!("SKIP: needs root to run a client as another user");
            return;
        }
        let r = rig("peer");
        // file permissions would keep other users out; open them so that only the uid check is left
        let dir = r.socket.parent().unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&r.socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        // the other user must be able to run the test binary wherever the build put it
        let exe = dir.join("test-binary");
        let me = std::env::current_exe().unwrap();
        std::fs::hard_link(&me, &exe)
            .or_else(|_| std::fs::copy(&me, &exe).map(|_| ()))
            .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = r.socket.clone();
        let out = r.run(move || {
            std::process::Command::new(exe)
                .args(["--exact", "ipc::tests::other_users_client", "--nocapture"])
                .env("LW_TEST_PEER_SOCKET", socket)
                .uid(65534)
                .gid(65534)
                .output()
        });
        let out = out.expect("could not run the client as another user");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("PEER-REPLY-BYTES=0"),
            "the other user's client did not run, or was answered:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            r.seen.borrow().is_empty(),
            "another user's request was executed"
        );
        assert_eq!(
            PANICS.load(std::sync::atomic::Ordering::SeqCst),
            panics_before,
            "the peer check panicked instead of refusing"
        );
        // and our own requests still work
        assert_eq!(r.ask(Flags::SHOW), Response::ok());
    }
}
