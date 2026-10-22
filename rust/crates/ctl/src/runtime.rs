//! Where the daemon's socket lives, and the checks that make it safe to talk to.
//!
//! The socket is only as trustworthy as the directory it sits in, so the directory is verified
//! before anything is sent: a real directory (not a symlink), owned by us, accessible by nobody
//! else. The socket itself must be a socket owned by us with no group/other permissions. All the
//! checks use `lstat`, never following links.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use llm_wrapper_protocol::InstanceName;

/// `sun_path` holds 108 bytes including the terminating NUL.
const MAX_SOCKET_PATH: usize = 107;

/// Why the runtime directory or the socket cannot be trusted. Every variant is a *security*
/// refusal or a setup problem; none of them is ever worked around.
#[derive(Debug)]
pub enum RuntimeError {
    /// `XDG_RUNTIME_DIR` is not an absolute path.
    NotAbsolute(PathBuf),
    /// The directory does not exist.
    Missing(PathBuf),
    /// The path exists but is not a directory.
    NotADirectory(PathBuf),
    /// The path is a symbolic link.
    Symlink(PathBuf),
    /// Owned by somebody else.
    WrongOwner {
        /// Offending path.
        path: PathBuf,
        /// Its owner.
        owner: u32,
        /// Our uid.
        expected: u32,
    },
    /// Group or other permission bits are set.
    TooPermissive {
        /// Offending path.
        path: PathBuf,
        /// Its permission bits.
        mode: u32,
    },
    /// The socket path does not fit in `sun_path`.
    PathTooLong(PathBuf),
    /// The socket path exists but is not a socket.
    NotASocket(PathBuf),
    /// Could not determine our own uid.
    NoUid(io::Error),
    /// Any other I/O failure while inspecting a path.
    Io(PathBuf, io::Error),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute(p) => write!(f, "XDG_RUNTIME_DIR must be an absolute path, got '{}'", p.display()),
            Self::Missing(p) => write!(f, "runtime directory '{}' does not exist", p.display()),
            Self::NotADirectory(p) => write!(f, "'{}' is not a directory", p.display()),
            Self::Symlink(p) => write!(f, "refusing '{}': it is a symbolic link", p.display()),
            Self::WrongOwner { path, owner, expected } => write!(
                f,
                "refusing '{}': owned by uid {owner}, not by you (uid {expected})",
                path.display()
            ),
            Self::TooPermissive { path, mode } => write!(
                f,
                "refusing '{}': mode {:04o} lets other users in (it must be 0700 for a directory, 0600 for the socket)",
                path.display(),
                mode & 0o7777
            ),
            Self::PathTooLong(p) => write!(f, "socket path '{}' is too long for a unix socket (max {MAX_SOCKET_PATH} bytes)", p.display()),
            Self::NotASocket(p) => write!(f, "refusing '{}': exists but is not a socket", p.display()),
            Self::NoUid(e) => write!(f, "cannot determine the current user (/proc/self): {e}"),
            Self::Io(p, e) => write!(f, "cannot inspect '{}': {e}", p.display()),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Our effective uid, read from the owner of `/proc/self` (no `unsafe`, no libc). If the kernel
/// reports something unexpected this fails closed.
pub fn current_uid() -> Result<u32, RuntimeError> {
    fs::metadata("/proc/self")
        .map(|m| m.uid())
        .map_err(RuntimeError::NoUid)
}

/// `$XDG_RUNTIME_DIR`, or the systemd default `/run/user/UID` when it is unset or empty.
pub fn runtime_dir(xdg: Option<&OsStr>, uid: u32) -> PathBuf {
    match xdg {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(format!("/run/user/{uid}")),
    }
}

/// Verify the runtime directory: absolute, a real directory, ours, mode `0700`-ish.
pub fn verify_runtime_dir(dir: &Path, uid: u32) -> Result<(), RuntimeError> {
    if !dir.is_absolute() {
        return Err(RuntimeError::NotAbsolute(dir.to_owned()));
    }
    let meta = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(RuntimeError::Missing(dir.to_owned()))
        }
        Err(e) => return Err(RuntimeError::Io(dir.to_owned(), e)),
    };
    if meta.file_type().is_symlink() {
        return Err(RuntimeError::Symlink(dir.to_owned()));
    }
    if !meta.is_dir() {
        return Err(RuntimeError::NotADirectory(dir.to_owned()));
    }
    check_owner_and_mode(dir, &meta, uid)
}

/// Socket file name for an instance: `llm-wrapper.v1.sock` or `llm-wrapper-NAME.v1.sock`.
/// The version in the name keeps incompatible daemons (notably the legacy Python one, which would
/// misread binary frames as command-line arguments) from ever receiving a message they cannot parse.
pub fn socket_path(dir: &Path, instance: Option<&InstanceName>) -> Result<PathBuf, RuntimeError> {
    let name = match instance {
        Some(i) => format!("llm-wrapper-{i}.v1.sock"),
        None => "llm-wrapper.v1.sock".to_owned(),
    };
    let path = dir.join(name);
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(RuntimeError::PathTooLong(path));
    }
    Ok(path)
}

/// What is at the socket path.
#[derive(Debug, PartialEq, Eq)]
pub enum SocketState {
    /// Nothing there: no daemon.
    Absent,
    /// A socket owned by us with private permissions.
    Present,
}

/// Inspect the socket path without following links.
pub fn verify_socket(path: &Path, uid: u32) -> Result<SocketState, RuntimeError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(SocketState::Absent),
        Err(e) => return Err(RuntimeError::Io(path.to_owned(), e)),
    };
    if !meta.file_type().is_socket() {
        return Err(RuntimeError::NotASocket(path.to_owned()));
    }
    check_owner_and_mode(path, &meta, uid)?;
    Ok(SocketState::Present)
}

fn check_owner_and_mode(path: &Path, meta: &fs::Metadata, uid: u32) -> Result<(), RuntimeError> {
    if meta.uid() != uid {
        return Err(RuntimeError::WrongOwner {
            path: path.to_owned(),
            owner: meta.uid(),
            expected: uid,
        });
    }
    if meta.mode() & 0o077 != 0 {
        return Err(RuntimeError::TooPermissive {
            path: path.to_owned(),
            mode: meta.mode(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener;

    fn tmp(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lw-rt-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        p
    }

    fn uid() -> u32 {
        current_uid().unwrap()
    }

    #[test]
    fn runtime_dir_resolution() {
        assert_eq!(
            runtime_dir(Some(OsStr::new("/run/x")), 5),
            PathBuf::from("/run/x")
        );
        assert_eq!(runtime_dir(None, 1000), PathBuf::from("/run/user/1000"));
        assert_eq!(
            runtime_dir(Some(OsStr::new("")), 1000),
            PathBuf::from("/run/user/1000")
        );
    }

    #[test]
    fn a_private_directory_passes() {
        let d = tmp("ok");
        assert!(verify_runtime_dir(&d, uid()).is_ok());
    }

    #[test]
    fn refuses_unsafe_directories() {
        let u = uid();
        let d = tmp("bad");
        assert!(matches!(
            verify_runtime_dir(Path::new("relative/dir"), u),
            Err(RuntimeError::NotAbsolute(_))
        ));
        assert!(matches!(
            verify_runtime_dir(&d.join("missing"), u),
            Err(RuntimeError::Missing(_))
        ));

        let file = d.join("file");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            verify_runtime_dir(&file, u),
            Err(RuntimeError::NotADirectory(_))
        ));

        let link = d.join("link");
        symlink(&d, &link).unwrap();
        assert!(matches!(
            verify_runtime_dir(&link, u),
            Err(RuntimeError::Symlink(_))
        ));

        for mode in [0o755, 0o750, 0o701, 0o770, 0o777] {
            let open = d.join(format!("open{mode:o}"));
            fs::create_dir(&open).unwrap();
            fs::set_permissions(&open, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                matches!(
                    verify_runtime_dir(&open, u),
                    Err(RuntimeError::TooPermissive { .. })
                ),
                "{mode:o}"
            );
        }
        assert!(matches!(
            verify_runtime_dir(&d, u + 1),
            Err(RuntimeError::WrongOwner { .. })
        ));
    }

    #[test]
    fn socket_path_names_and_length() {
        let d = Path::new("/run/user/1000");
        assert_eq!(
            socket_path(d, None).unwrap(),
            PathBuf::from("/run/user/1000/llm-wrapper.v1.sock")
        );
        let i = InstanceName::parse("work").unwrap();
        assert_eq!(
            socket_path(d, Some(&i)).unwrap(),
            PathBuf::from("/run/user/1000/llm-wrapper-work.v1.sock")
        );
        let long = Path::new("/very/long/runtime/directory/that/keeps/going/and/going/and/going/and/going/and/going/and/going/xx");
        assert!(matches!(
            socket_path(long, None),
            Err(RuntimeError::PathTooLong(_))
        ));
    }

    #[test]
    fn socket_states() {
        let u = uid();
        let d = tmp("sock");
        let s = d.join("s.sock");
        assert_eq!(verify_socket(&s, u).unwrap(), SocketState::Absent);

        let listener = UnixListener::bind(&s).unwrap();
        fs::set_permissions(&s, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(verify_socket(&s, u).unwrap(), SocketState::Present);
        fs::set_permissions(&s, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(matches!(
            verify_socket(&s, u),
            Err(RuntimeError::TooPermissive { .. })
        ));
        fs::set_permissions(&s, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            verify_socket(&s, u + 1),
            Err(RuntimeError::WrongOwner { .. })
        ));
        drop(listener);

        let reg = d.join("regular");
        fs::write(&reg, b"x").unwrap();
        assert!(matches!(
            verify_socket(&reg, u),
            Err(RuntimeError::NotASocket(_))
        ));
        let link = d.join("link.sock");
        symlink(&s, &link).unwrap();
        assert!(matches!(
            verify_socket(&link, u),
            Err(RuntimeError::NotASocket(_))
        ));
    }

    #[test]
    fn messages_tell_the_user_what_to_do() {
        let e = RuntimeError::TooPermissive {
            path: PathBuf::from("/x"),
            mode: 0o40755,
        };
        let m = e.to_string();
        assert!(m.contains("0755") && m.contains("0700"), "{m}");
    }
}
