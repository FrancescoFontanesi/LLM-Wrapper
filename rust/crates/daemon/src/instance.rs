//! One daemon per instance name, enforced without races.
//!
//! The first process takes an exclusive `flock` on `llm-wrapper[-NAME].lock`; the kernel releases it
//! when that process dies for any reason, so there are no stale locks. Whoever holds the lock may
//! remove a leftover socket and bind a new one — a live daemon's socket is never touched. A process
//! that finds the lock taken does not start anything: it hands its request to the running daemon.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use llm_wrapper_platform::client::{self, ClientError};
use llm_wrapper_platform::runtime::{self, SocketState};
use llm_wrapper_protocol::{InstanceName, Request, Response};

fn lock_path(runtime_dir: &Path, instance: Option<&InstanceName>) -> PathBuf {
    runtime_dir.join(match instance {
        Some(i) => format!("llm-wrapper-{i}.lock"),
        None => "llm-wrapper.lock".to_owned(),
    })
}

/// Take the instance lock (without waiting). `runtime_dir` must already have been verified.
pub fn take_lock(
    runtime_dir: &Path,
    instance: Option<&InstanceName>,
    uid: u32,
) -> Result<Option<(File, PathBuf)>, String> {
    let path = lock_path(runtime_dir, instance);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot open the lock file {}: {e}", path.display()))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() || meta.uid() != uid {
        return Err(format!(
            "refusing the lock file {}: not a regular file of yours",
            path.display()
        ));
    }
    match file.try_lock() {
        Ok(()) => Ok(Some((file, path))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

/// Bind the control socket. We hold the lock, so any socket already there belongs to a dead daemon;
/// it is removed only if it really is a socket of ours (anything else is refused, not deleted).
pub fn bind_socket(socket: &Path, uid: u32) -> Result<UnixListener, String> {
    match fs::symlink_metadata(socket) {
        Ok(meta) => {
            if !meta.file_type().is_socket() || meta.uid() != uid {
                return Err(format!(
                    "refusing to replace {}: it is not a socket of yours",
                    socket.display()
                ));
            }
            fs::remove_file(socket)
                .map_err(|e| format!("cannot remove the stale socket {}: {e}", socket.display()))?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect {}: {e}", socket.display())),
    }
    let listener =
        UnixListener::bind(socket).map_err(|e| format!("cannot bind {}: {e}", socket.display()))?;
    // The directory is 0700 so nobody could have got in, but the socket should not depend on it.
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot restrict {}: {e}", socket.display()))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(listener)
}

/// Hand `request` to the daemon that owns the instance and return its answer. The daemon may still
/// be starting (it holds the lock before it listens), so a missing socket is waited for.
pub fn forward(
    socket: &Path,
    uid: u32,
    request: &Request,
    patience: Duration,
) -> Result<Response, String> {
    let deadline = Instant::now() + patience;
    loop {
        match runtime::verify_socket(socket, uid).map_err(|e| e.to_string())? {
            SocketState::Present => match client::connect(socket, client::TIMEOUT)
                .and_then(|s| client::exchange(&s, request, client::TIMEOUT))
            {
                Ok(response) => return Ok(response),
                Err(ClientError::NotRunning) => {}
                Err(e) => return Err(e.to_string()),
            },
            SocketState::Absent => {}
        }
        if Instant::now() >= deadline {
            return Err("the running daemon did not answer".to_owned());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lwd-inst-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        p
    }

    fn uid() -> u32 {
        runtime::current_uid().unwrap()
    }

    #[test]
    fn the_second_process_does_not_get_the_instance() {
        let d = dir();
        let first = take_lock(&d, None, uid()).unwrap();
        assert!(first.is_some());
        assert!(
            take_lock(&d, None, uid()).unwrap().is_none(),
            "the lock is exclusive"
        );
        // another instance name is independent
        let other = InstanceName::parse("work").unwrap();
        assert!(take_lock(&d, Some(&other), uid()).unwrap().is_some());
        // and the lock is gone with its holder: no stale locks
        drop(first);
        assert!(take_lock(&d, None, uid()).unwrap().is_some());
    }

    #[test]
    fn the_socket_is_private_and_a_dead_daemons_socket_is_replaced() {
        let d = dir();
        let socket = runtime::socket_path(&d, None).unwrap();
        let listener = bind_socket(&socket, uid()).unwrap();
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // the daemon dies: the socket file is left behind
        drop(listener);
        assert!(socket.exists());
        drop(bind_socket(&socket, uid()).unwrap());
    }

    #[test]
    fn something_that_is_not_our_socket_is_never_deleted() {
        let d = dir();
        let socket = runtime::socket_path(&d, None).unwrap();
        fs::write(&socket, b"precious").unwrap();
        assert!(bind_socket(&socket, uid())
            .err()
            .unwrap()
            .contains("not a socket of yours"));
        assert_eq!(fs::read(&socket).unwrap(), b"precious");

        let d = dir();
        let socket = runtime::socket_path(&d, None).unwrap();
        let target = d.join("elsewhere");
        fs::write(&target, b"precious").unwrap();
        symlink(&target, &socket).unwrap();
        assert!(bind_socket(&socket, uid()).is_err());
        assert!(target.exists());
        // a socket that belongs to somebody else is not ours to remove either
        let d = dir();
        let socket = runtime::socket_path(&d, None).unwrap();
        drop(UnixListener::bind(&socket).unwrap());
        assert!(bind_socket(&socket, uid() + 1).is_err());
        assert!(socket.exists());
    }

    #[test]
    fn the_lock_file_must_be_ours() {
        let d = dir();
        // a lock file that is really a directory
        fs::create_dir(d.join("llm-wrapper.lock")).unwrap();
        assert!(take_lock(&d, None, uid()).is_err());
        // a lock file that belongs to someone else
        let d = dir();
        fs::write(d.join("llm-wrapper.lock"), b"").unwrap();
        assert!(take_lock(&d, None, uid() + 1)
            .unwrap_err()
            .contains("not a regular file of yours"));
    }

    #[test]
    fn forwarding_waits_for_a_daemon_that_is_still_starting() {
        let d = dir();
        let socket = runtime::socket_path(&d, None).unwrap();
        let s2 = socket.clone();
        let server = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            let l = UnixListener::bind(&s2).unwrap();
            fs::set_permissions(&s2, fs::Permissions::from_mode(0o600)).unwrap();
            let (mut c, _) = l.accept().unwrap();
            llm_wrapper_protocol::frame::read(&mut c, llm_wrapper_protocol::frame::Kind::Request)
                .unwrap();
            io::Write::write_all(&mut c, &Response::ok().encode().unwrap()).unwrap();
        });
        let r = forward(&socket, uid(), &Request::default(), Duration::from_secs(5)).unwrap();
        assert_eq!(r, Response::ok());
        server.join().unwrap();
        // and it gives up politely
        let d = dir();
        let socket = runtime::socket_path(&d, None).unwrap();
        let err = forward(
            &socket,
            uid(),
            &Request::default(),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
    }
}
