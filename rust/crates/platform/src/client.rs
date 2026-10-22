//! Talking to a running daemon: connect, send one request frame, read one response frame, all
//! under hard deadlines so a hung daemon can never hang a hotkey.

use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::{ProtocolError, Request, Response};

/// Deadline for connecting, and (separately) for the whole reply. Same as the legacy client.
pub const TIMEOUT: Duration = Duration::from_millis(800);

/// Why a request did not get an answer.
#[derive(Debug)]
pub enum ClientError {
    /// Nobody is listening (no socket, or a stale one).
    NotRunning,
    /// The daemon did not answer in time (`connect` or `reply`).
    Timeout(&'static str),
    /// Transport failure.
    Io(io::Error),
    /// The daemon answered with something that is not valid protocol.
    Protocol(ProtocolError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => f.write_str("the daemon is not running"),
            Self::Timeout(what) => write!(
                f,
                "the daemon did not answer ({what} timed out after {} ms)",
                TIMEOUT.as_millis()
            ),
            Self::Io(e) => write!(f, "cannot talk to the daemon: {e}"),
            Self::Protocol(e) => write!(f, "unexpected reply from the daemon: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Connect with a deadline. `UnixStream::connect` can block forever on a daemon whose accept queue
/// is full, and std has no timeout for it, so the attempt runs on a helper thread that is simply
/// abandoned (the process exits right after) if it does not finish in time.
pub fn connect(path: &Path, timeout: Duration) -> Result<UnixStream, ClientError> {
    let (tx, rx) = mpsc::channel();
    let target = path.to_owned();
    thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(target));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e))
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err(ClientError::NotRunning)
        }
        Ok(Err(e)) => Err(ClientError::Io(e)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(ClientError::Timeout("connect")),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(ClientError::Io(io::Error::other("connect thread died")))
        }
    }
}

/// A reader whose *total* time is bounded: every `read` gets only what is left of the deadline, so
/// a peer that trickles one byte at a time cannot stretch a 0.8 s budget into minutes.
struct DeadlineReader<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut s = self.stream;
        s.read(buf)
    }
}

/// Send `request` and wait for the daemon's verdict.
pub fn exchange(
    stream: &UnixStream,
    request: &Request,
    timeout: Duration,
) -> Result<Response, ClientError> {
    let bytes = request.encode().map_err(ClientError::Protocol)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(ClientError::Io)?;
    let mut writer = stream;
    writer.write_all(&bytes).map_err(timeout_or_io)?;

    let mut reader = DeadlineReader {
        stream,
        deadline: Instant::now() + timeout,
    };
    let body = frame::read(&mut reader, Kind::Response).map_err(|e| match e {
        ProtocolError::Io(io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
            ClientError::Timeout("reply")
        }
        other => ClientError::Protocol(other),
    })?;
    Response::decode(&body).map_err(ClientError::Protocol)
}

fn timeout_or_io(e: io::Error) -> ClientError {
    if matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        ClientError::Timeout("send")
    } else {
        ClientError::Io(e)
    }
}
