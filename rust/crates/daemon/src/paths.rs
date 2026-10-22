//! Where the daemon keeps things, following the XDG Base Directory specification, and the checks
//! that make those places safe.
//!
//! * data   `$XDG_DATA_HOME/llm-wrapper`   — login cookies and web storage
//! * cache  `$XDG_CACHE_HOME/llm-wrapper`  — `WebKit`'s disk cache
//! * config `$XDG_CONFIG_HOME/llm-wrapper` — `providers.json`
//!
//! An `--instance NAME` gets its own data and cache under `…/llm-wrapper/instances/NAME`; the
//! provider list is shared. A relative or empty XDG variable is ignored, as the specification says.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use llm_wrapper_platform::private::{ensure_private_dir, DirOutcome};
use llm_wrapper_platform::runtime;
use llm_wrapper_protocol::InstanceName;

const APP_DIR: &str = "llm-wrapper";

/// The environment variables that matter, captured once so resolution is a pure function.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub home: Option<OsString>,
    pub data_home: Option<OsString>,
    pub cache_home: Option<OsString>,
    pub config_home: Option<OsString>,
    pub runtime_dir: Option<OsString>,
}

impl Env {
    /// Read the process environment.
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var_os(k);
        Self {
            home: var("HOME"),
            data_home: var("XDG_DATA_HOME"),
            cache_home: var("XDG_CACHE_HOME"),
            config_home: var("XDG_CONFIG_HOME"),
            runtime_dir: var("XDG_RUNTIME_DIR"),
        }
    }
}

/// Resolved locations.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub runtime: PathBuf,
    /// Root of the data tree, shared by all instances.
    pub data_root: PathBuf,
    /// Root of the cache tree, shared by all instances.
    pub cache_root: PathBuf,
    /// Where this instance keeps its data.
    pub data: PathBuf,
    /// Where this instance keeps its cache.
    pub cache: PathBuf,
    pub config: PathBuf,
}

fn base(
    var: Option<&OsString>,
    home: Option<&OsString>,
    default: &str,
    what: &str,
) -> Result<PathBuf, String> {
    if let Some(v) = var.filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            return Ok(p);
        }
    }
    match home.map(PathBuf::from).filter(|h| h.is_absolute()) {
        Some(h) => Ok(h.join(default)),
        None => Err(format!("cannot place the {what} directory: neither a usable XDG variable nor an absolute $HOME is set")),
    }
}

/// Work out every location. Nothing is touched on disk.
pub fn resolve(env: &Env, uid: u32, instance: Option<&InstanceName>) -> Result<Dirs, String> {
    let data_root = base(
        env.data_home.as_ref(),
        env.home.as_ref(),
        ".local/share",
        "data",
    )?
    .join(APP_DIR);
    let cache_root = base(
        env.cache_home.as_ref(),
        env.home.as_ref(),
        ".cache",
        "cache",
    )?
    .join(APP_DIR);
    let config = base(
        env.config_home.as_ref(),
        env.home.as_ref(),
        ".config",
        "config",
    )?
    .join(APP_DIR);
    let (data, cache) = match instance {
        Some(i) => (
            data_root.join("instances").join(i.as_str()),
            cache_root.join("instances").join(i.as_str()),
        ),
        None => (data_root.clone(), cache_root.clone()),
    };
    Ok(Dirs {
        runtime: runtime::runtime_dir(env.runtime_dir.as_deref(), uid),
        data_root,
        cache_root,
        data,
        cache,
        config,
    })
}

/// Create or verify every directory: private (`0700`), ours, not a link. The runtime directory must
/// already exist and be private (it belongs to the session, we do not repair it). Returns remarks
/// for the log (directories that had to be created or tightened).
pub fn prepare(dirs: &Dirs, uid: u32) -> Result<Vec<String>, String> {
    runtime::verify_runtime_dir(&dirs.runtime, uid).map_err(|e| e.to_string())?;
    let mut notes = Vec::new();
    let wanted: [&Path; 5] = [
        &dirs.data_root,
        &dirs.data,
        &dirs.cache_root,
        &dirs.cache,
        &dirs.config,
    ];
    for dir in wanted {
        match ensure_private_dir(dir, uid).map_err(|e| e.to_string())? {
            DirOutcome::Created | DirOutcome::Existing => {}
            DirOutcome::Tightened { was } => {
                notes.push(format!("{} had mode {:04o}; now 0700", dir.display(), was));
            }
        }
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    // Option because that is what every `Env` field takes; the tests read better without `Some(..)`
    #[allow(clippy::unnecessary_wraps)]
    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    fn env() -> Env {
        Env {
            home: os("/home/u"),
            runtime_dir: os("/run/user/1000"),
            ..Env::default()
        }
    }

    #[test]
    fn defaults_follow_the_xdg_specification() {
        let d = resolve(&env(), 1000, None).unwrap();
        assert_eq!(d.data, PathBuf::from("/home/u/.local/share/llm-wrapper"));
        assert_eq!(d.cache, PathBuf::from("/home/u/.cache/llm-wrapper"));
        assert_eq!(d.config, PathBuf::from("/home/u/.config/llm-wrapper"));
        assert_eq!(d.runtime, PathBuf::from("/run/user/1000"));
    }

    #[test]
    fn xdg_variables_win_when_absolute_and_are_ignored_otherwise() {
        let e = Env {
            data_home: os("/d"),
            cache_home: os("relative/cache"),
            config_home: os(""),
            ..env()
        };
        let d = resolve(&e, 1000, None).unwrap();
        assert_eq!(d.data, PathBuf::from("/d/llm-wrapper"));
        assert_eq!(
            d.cache,
            PathBuf::from("/home/u/.cache/llm-wrapper"),
            "a relative XDG path is invalid"
        );
        assert_eq!(
            d.config,
            PathBuf::from("/home/u/.config/llm-wrapper"),
            "an empty one too"
        );
    }

    #[test]
    fn instances_get_their_own_data_and_cache_but_share_the_providers() {
        let i = InstanceName::parse("work").unwrap();
        let d = resolve(&env(), 1000, Some(&i)).unwrap();
        assert_eq!(
            d.data,
            PathBuf::from("/home/u/.local/share/llm-wrapper/instances/work")
        );
        assert_eq!(
            d.cache,
            PathBuf::from("/home/u/.cache/llm-wrapper/instances/work")
        );
        assert_eq!(d.config, PathBuf::from("/home/u/.config/llm-wrapper"));
        assert_eq!(
            d.data_root,
            PathBuf::from("/home/u/.local/share/llm-wrapper")
        );
    }

    #[test]
    fn without_a_usable_home_it_refuses_instead_of_guessing() {
        for home in [None, os(""), os("relative")] {
            let e = Env {
                home,
                ..Env::default()
            };
            assert!(
                resolve(&e, 1000, None).unwrap_err().contains("data"),
                "{e:?}"
            );
        }
        // ...unless every variable is given
        let e = Env {
            data_home: os("/d"),
            cache_home: os("/c"),
            config_home: os("/g"),
            ..Env::default()
        };
        assert!(resolve(&e, 1000, None).is_ok());
    }

    #[test]
    fn prepare_creates_private_directories_and_tightens_loose_ones() {
        let root = std::env::temp_dir().join(format!("lwd-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("rt")).unwrap();
        std::fs::set_permissions(root.join("rt"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = runtime::current_uid().unwrap();
        let i = InstanceName::parse("a").unwrap();
        let e = Env {
            home: os(root.to_str().unwrap()),
            runtime_dir: os(root.join("rt").to_str().unwrap()),
            ..Env::default()
        };
        let d = resolve(&e, uid, Some(&i)).unwrap();
        assert!(prepare(&d, uid).unwrap().is_empty());
        for p in [&d.data_root, &d.data, &d.cache_root, &d.cache, &d.config] {
            assert_eq!(
                std::fs::metadata(p).unwrap().mode() & 0o777,
                0o700,
                "{}",
                p.display()
            );
        }
        std::fs::set_permissions(&d.data_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let notes = prepare(&d, uid).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(
            std::fs::metadata(&d.data_root).unwrap().mode() & 0o777,
            0o700
        );

        // a link in place of a directory is refused
        std::fs::remove_dir_all(&d.config).unwrap();
        symlink(&d.cache, &d.config).unwrap();
        assert!(prepare(&d, uid).unwrap_err().contains("symbolic link"));
        // and so is an unsafe runtime directory
        std::fs::set_permissions(root.join("rt"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(&d, uid).unwrap_err().contains("0700"));
    }
}
