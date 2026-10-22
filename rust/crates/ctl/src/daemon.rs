//! Starting the daemon when none is running.
//!
//! The daemon binary is *never* looked up through `PATH` or any environment variable: it is the
//! file named `llm-wrapperd` in the same directory as this executable, and it is only run after
//! it, its directory and every ancestor have been checked. Anything that would let another user
//! swap the binary between install and exec (symlink, foreign owner, group/other-writable) is
//! refused.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// File name of the daemon, next to the client.
pub const DAEMON_NAME: &str = "llm-wrapperd";

const STICKY: u32 = 0o1000;
const GROUP_OTHER_WRITE: u32 = 0o022;

/// Why the daemon cannot be started.
#[derive(Debug)]
pub enum DaemonError {
    /// The path of this executable is unknown.
    NoExePath,
    /// No `llm-wrapperd` next to the client.
    NotFound(PathBuf),
    /// It is there but not trustworthy.
    Untrusted {
        /// What was checked.
        path: PathBuf,
        /// Why it failed.
        why: &'static str,
    },
    /// Could not inspect a path.
    Io(PathBuf, io::Error),
    /// `exec` itself failed.
    Exec(PathBuf, io::Error),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoExePath => f.write_str(
                "cannot determine where llm-wrapper is installed, so the daemon cannot be started",
            ),
            Self::NotFound(p) => write!(
                f,
                "the daemon '{}' is not installed next to llm-wrapper",
                p.display()
            ),
            Self::Untrusted { path, why } => {
                write!(f, "refusing to start '{}': {why}", path.display())
            }
            Self::Io(p, e) => write!(f, "cannot inspect '{}': {e}", p.display()),
            Self::Exec(p, e) => write!(f, "cannot start '{}': {e}", p.display()),
        }
    }
}

impl std::error::Error for DaemonError {}

fn trusted_owner(meta: &fs::Metadata, uid: u32) -> bool {
    meta.uid() == uid || meta.uid() == 0
}

fn check_dir(path: &Path, uid: u32, strict: bool) -> Result<(), DaemonError> {
    let untrusted = |why| DaemonError::Untrusted {
        path: path.to_owned(),
        why,
    };
    let meta = fs::symlink_metadata(path).map_err(|e| DaemonError::Io(path.to_owned(), e))?;
    if meta.file_type().is_symlink() {
        return Err(untrusted("directory is a symbolic link"));
    }
    if !meta.is_dir() {
        return Err(untrusted("not a directory"));
    }
    if !trusted_owner(&meta, uid) {
        return Err(untrusted("directory is owned by another user"));
    }
    // The installation directory must not be writable by anyone else. Further up, a world-writable
    // directory is tolerated only when sticky (like /tmp): there entries cannot be renamed away.
    let writable_by_others = meta.mode() & GROUP_OTHER_WRITE != 0;
    if writable_by_others && (strict || meta.mode() & STICKY == 0) {
        return Err(untrusted("directory is writable by other users"));
    }
    Ok(())
}

/// Find and vet the daemon that belongs to the executable at `exe`.
pub fn locate(exe: &Path, uid: u32) -> Result<PathBuf, DaemonError> {
    let dir = exe.parent().ok_or(DaemonError::NoExePath)?;
    let daemon = dir.join(DAEMON_NAME);
    let untrusted = |why| DaemonError::Untrusted {
        path: daemon.clone(),
        why,
    };

    let meta = match fs::symlink_metadata(&daemon) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(DaemonError::NotFound(daemon)),
        Err(e) => return Err(DaemonError::Io(daemon, e)),
    };
    if meta.file_type().is_symlink() {
        return Err(untrusted("it is a symbolic link"));
    }
    if !meta.is_file() {
        return Err(untrusted("it is not a regular file"));
    }
    if !trusted_owner(&meta, uid) {
        return Err(untrusted("it is owned by another user"));
    }
    if meta.mode() & GROUP_OTHER_WRITE != 0 {
        return Err(untrusted("it is writable by other users"));
    }
    if meta.mode() & 0o111 == 0 {
        return Err(untrusted("it is not executable"));
    }

    check_dir(dir, uid, true)?;
    for ancestor in dir.ancestors().skip(1) {
        check_dir(ancestor, uid, false)?;
    }
    Ok(daemon)
}

/// Replace this process with the daemon, passing the original arguments through untouched. Only
/// returns if that failed.
pub fn exec(daemon: &Path, args: &[String]) -> DaemonError {
    let err = Command::new(daemon).arg0(DAEMON_NAME).args(args).exec();
    DaemonError::Exec(daemon.to_owned(), err)
}
