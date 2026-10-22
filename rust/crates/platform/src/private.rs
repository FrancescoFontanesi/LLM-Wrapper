//! Files and directories that only the user can reach.
//!
//! Everything the daemon keeps — login cookies, caches, the provider list, the state file — goes
//! through these helpers: directories are created `0700`, files `0600`, nothing is followed through a
//! symbolic link, ownership is checked, and replacement is atomic (write a sibling, then rename).

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Why a private path was refused.
#[derive(Debug)]
pub enum PrivateError {
    /// The path is not absolute.
    NotAbsolute(PathBuf),
    /// The path (or the file about to be replaced) is a symbolic link.
    Symlink(PathBuf),
    /// Expected a directory.
    NotADirectory(PathBuf),
    /// Expected a regular file.
    NotARegularFile(PathBuf),
    /// Owned by another user.
    WrongOwner {
        /// Offending path.
        path: PathBuf,
        /// Its owner.
        owner: u32,
        /// Our uid.
        expected: u32,
    },
    /// Group- or other-writable: somebody else could have changed it.
    WritableByOthers {
        /// Offending path.
        path: PathBuf,
        /// Its permission bits.
        mode: u32,
    },
    /// Bigger than the limit the caller is willing to read.
    TooLarge {
        /// Offending path.
        path: PathBuf,
        /// The limit in bytes.
        limit: u64,
    },
    /// The file changed between the check and the open.
    Raced(PathBuf),
    /// Any other I/O failure.
    Io(PathBuf, io::Error),
}

impl fmt::Display for PrivateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute(p) => write!(f, "'{}' is not an absolute path", p.display()),
            Self::Symlink(p) => write!(f, "refusing '{}': it is a symbolic link", p.display()),
            Self::NotADirectory(p) => write!(f, "'{}' is not a directory", p.display()),
            Self::NotARegularFile(p) => write!(f, "'{}' is not a regular file", p.display()),
            Self::WrongOwner {
                path,
                owner,
                expected,
            } => {
                write!(
                    f,
                    "refusing '{}': owned by uid {owner}, not by you (uid {expected})",
                    path.display()
                )
            }
            Self::WritableByOthers { path, mode } => {
                write!(
                    f,
                    "refusing '{}': mode {:04o} lets other users change it",
                    path.display(),
                    mode & 0o7777
                )
            }
            Self::TooLarge { path, limit } => {
                write!(f, "'{}' is larger than {limit} bytes", path.display())
            }
            Self::Raced(p) => write!(f, "'{}' changed while it was being opened", p.display()),
            Self::Io(p, e) => write!(f, "'{}': {e}", p.display()),
        }
    }
}

impl std::error::Error for PrivateError {}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> PrivateError + '_ {
    move |e| PrivateError::Io(path.to_owned(), e)
}

/// What [`ensure_private_dir`] had to do.
#[derive(Debug, PartialEq, Eq)]
pub enum DirOutcome {
    /// Did not exist; created `0700`.
    Created,
    /// Already private.
    Existing,
    /// Existed with group/other access; permissions were reduced to `0700`.
    Tightened {
        /// The mode it had.
        was: u32,
    },
}

/// Make sure `path` is a directory that is ours and `0700`, creating it (and missing parents,
/// also `0700`) if needed. A symbolic link or a directory owned by somebody else is refused;
/// loose permissions on our own directory are tightened. Existing ancestors are left alone.
pub fn ensure_private_dir(path: &Path, uid: u32) -> Result<DirOutcome, PrivateError> {
    if !path.is_absolute() {
        return Err(PrivateError::NotAbsolute(path.to_owned()));
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(PrivateError::Symlink(path.to_owned()));
            }
            if !meta.is_dir() {
                return Err(PrivateError::NotADirectory(path.to_owned()));
            }
            if meta.uid() != uid {
                return Err(PrivateError::WrongOwner {
                    path: path.to_owned(),
                    owner: meta.uid(),
                    expected: uid,
                });
            }
            if meta.mode() & 0o077 != 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(io_err(path))?;
                return Ok(DirOutcome::Tightened {
                    was: meta.mode() & 0o7777,
                });
            }
            Ok(DirOutcome::Existing)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(io_err(path))?;
            // `recursive` tolerates a directory that appeared meanwhile: check what we ended up with
            let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
            if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != uid {
                return Err(PrivateError::Raced(path.to_owned()));
            }
            if meta.mode() & 0o077 != 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(io_err(path))?;
            }
            Ok(DirOutcome::Created)
        }
        Err(e) => Err(PrivateError::Io(path.to_owned(), e)),
    }
}

/// Replace `path` with `contents`, atomically, as a `0600` file.
///
/// The data goes to `<path>.tmp` (any leftover from a crash is removed first; the temporary is
/// created exclusively, so a planted symlink is never followed) and is then renamed over `path`.
/// A symbolic link at `path` is replaced, not written through. With `durable` the file is
/// flushed to disk before the rename.
pub fn write_private_atomic(
    path: &Path,
    contents: &[u8],
    durable: bool,
) -> Result<(), PrivateError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(PrivateError::Io(tmp, e)),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(io_err(&tmp))?;
    let written = file
        .write_all(contents)
        .and_then(|()| if durable { file.sync_all() } else { Ok(()) });
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(PrivateError::Io(tmp, e));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        PrivateError::Io(path.to_owned(), e)
    })
}

/// Read a regular file that belongs to us and that nobody else can have modified.
///
/// `Ok(None)` if it does not exist. Refused: symbolic links, anything that is not a regular file,
/// files owned by someone else, group/other-writable files, files above `max` bytes. (Readable by
/// others is accepted: the provider list holds no secret, and users create it by hand.)
pub fn read_private_file(path: &Path, uid: u32, max: u64) -> Result<Option<Vec<u8>>, PrivateError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PrivateError::Io(path.to_owned(), e)),
    };
    if meta.file_type().is_symlink() {
        return Err(PrivateError::Symlink(path.to_owned()));
    }
    if !meta.is_file() {
        return Err(PrivateError::NotARegularFile(path.to_owned()));
    }
    if meta.uid() != uid {
        return Err(PrivateError::WrongOwner {
            path: path.to_owned(),
            owner: meta.uid(),
            expected: uid,
        });
    }
    if meta.mode() & 0o022 != 0 {
        return Err(PrivateError::WritableByOthers {
            path: path.to_owned(),
            mode: meta.mode(),
        });
    }
    if meta.len() > max {
        return Err(PrivateError::TooLarge {
            path: path.to_owned(),
            limit: max,
        });
    }
    let file = File::open(path).map_err(io_err(path))?;
    // the path could have been swapped between lstat and open: insist on the same file
    let opened = file.metadata().map_err(io_err(path))?;
    if opened.dev() != meta.dev() || opened.ino() != meta.ino() {
        return Err(PrivateError::Raced(path.to_owned()));
    }
    let mut data = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut data)
        .map_err(io_err(path))?;
    if data.len() as u64 > max {
        return Err(PrivateError::TooLarge {
            path: path.to_owned(),
            limit: max,
        });
    }
    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::current_uid;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn tmp(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lwp-{}-{}-{label}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn mode(p: &Path) -> u32 {
        fs::symlink_metadata(p).unwrap().mode() & 0o7777
    }

    fn uid() -> u32 {
        current_uid().unwrap()
    }

    #[test]
    fn creates_nested_directories_private() {
        let base = tmp("create");
        let leaf = base.join("a/b/c");
        assert_eq!(
            ensure_private_dir(&leaf, uid()).unwrap(),
            DirOutcome::Created
        );
        for d in ["a", "a/b", "a/b/c"] {
            assert_eq!(mode(&base.join(d)), 0o700, "{d}");
        }
        assert_eq!(
            ensure_private_dir(&leaf, uid()).unwrap(),
            DirOutcome::Existing
        );
    }

    #[test]
    fn tightens_our_own_loose_directory_but_leaves_ancestors_alone() {
        let base = tmp("tighten");
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        let d = base.join("data");
        fs::create_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            ensure_private_dir(&d, uid()).unwrap(),
            DirOutcome::Tightened { was: 0o755 }
        );
        assert_eq!(mode(&d), 0o700);
        assert_eq!(
            mode(&base),
            0o755,
            "an existing ancestor is not ours to change"
        );
    }

    #[test]
    fn refuses_links_files_relative_paths_and_foreign_owners() {
        let base = tmp("refuse");
        let real = base.join("real");
        fs::create_dir(&real).unwrap();
        let link = base.join("link");
        symlink(&real, &link).unwrap();
        assert!(matches!(
            ensure_private_dir(&link, uid()),
            Err(PrivateError::Symlink(_))
        ));
        let file = base.join("file");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            ensure_private_dir(&file, uid()),
            Err(PrivateError::NotADirectory(_))
        ));
        assert!(matches!(
            ensure_private_dir(Path::new("relative"), uid()),
            Err(PrivateError::NotAbsolute(_))
        ));
        assert!(matches!(
            ensure_private_dir(&real, uid() + 1),
            Err(PrivateError::WrongOwner { .. })
        ));
    }

    #[test]
    fn atomic_write_is_private_complete_and_leaves_no_temporary() {
        let base = tmp("write");
        let f = base.join("state.json");
        write_private_atomic(&f, b"one", false).unwrap();
        assert_eq!((fs::read(&f).unwrap(), mode(&f)), (b"one".to_vec(), 0o600));
        write_private_atomic(&f, b"two two", true).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"two two");
        let names: Vec<_> = fs::read_dir(&base)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn atomic_write_replaces_a_symlink_instead_of_writing_through_it() {
        let base = tmp("symlink");
        let victim = base.join("victim");
        fs::write(&victim, b"precious").unwrap();
        let f = base.join("state.json");
        symlink(&victim, &f).unwrap();
        write_private_atomic(&f, b"new", false).unwrap();
        assert_eq!(
            fs::read(&victim).unwrap(),
            b"precious",
            "the link target must be untouched"
        );
        assert!(!fs::symlink_metadata(&f).unwrap().file_type().is_symlink());
    }

    #[test]
    fn a_planted_temporary_is_not_followed() {
        let base = tmp("planted");
        let victim = base.join("victim");
        fs::write(&victim, b"precious").unwrap();
        let f = base.join("providers.json");
        symlink(&victim, base.join("providers.json.tmp")).unwrap();
        write_private_atomic(&f, b"data", false).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert_eq!(fs::read(&f).unwrap(), b"data");
    }

    #[test]
    fn reading() {
        let base = tmp("read");
        let f = base.join("p.json");
        assert!(read_private_file(&f, uid(), 100).unwrap().is_none());
        fs::write(&f, b"hello").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            read_private_file(&f, uid(), 100).unwrap().unwrap(),
            b"hello"
        );
        assert!(matches!(
            read_private_file(&f, uid(), 4),
            Err(PrivateError::TooLarge { .. })
        ));
        assert!(matches!(
            read_private_file(&f, uid() + 1, 100),
            Err(PrivateError::WrongOwner { .. })
        ));
        for bad in [0o666, 0o662, 0o620, 0o664] {
            fs::set_permissions(&f, fs::Permissions::from_mode(bad)).unwrap();
            assert!(
                matches!(
                    read_private_file(&f, uid(), 100),
                    Err(PrivateError::WritableByOthers { .. })
                ),
                "{bad:o}"
            );
        }
        let link = base.join("link");
        symlink(&f, &link).unwrap();
        assert!(matches!(
            read_private_file(&link, uid(), 100),
            Err(PrivateError::Symlink(_))
        ));
        assert!(matches!(
            read_private_file(&base, uid(), 100),
            Err(PrivateError::NotARegularFile(_))
        ));
    }
}
