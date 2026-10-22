//! The persisted provider list (`providers.json`).
//!
//! Loading never loses anything silently. A file that cannot be trusted or parsed is moved aside
//! (`providers.json.corrupt-<time>`, `.untrusted-<time>`) before the built-in list takes over, and a
//! file with some unusable entries is copied (`providers.json.dropped-<time>`) before those entries
//! disappear from the next save. Every such decision comes back as a note for the log.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use llm_wrapper_platform::private::{read_private_file, write_private_atomic, PrivateError};
use llm_wrapper_protocol::{ProviderName, ProviderUrl};
use serde_json::Value;

use crate::core::{builtin_providers, Providers};

/// Largest provider file we are willing to read.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The list to use, and what happened on the way.
#[derive(Debug)]
pub struct Loaded {
    /// Never empty.
    pub providers: Providers,
    /// Human-readable remarks (for the log).
    pub notes: Vec<String>,
}

fn aside(path: &Path, kind: &str, now: u64) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 0..100 {
        let suffix = if n == 0 {
            String::new()
        } else {
            format!("-{n}")
        };
        let target = path.with_file_name(format!("{name}.{kind}-{now}{suffix}"));
        if fs::symlink_metadata(&target).is_err() {
            fs::rename(path, &target)?;
            return Ok(target);
        }
    }
    Err(io::Error::other("no free name for the backup"))
}

type Parsed = (Providers, Vec<String>);

fn parse(bytes: &[u8]) -> Result<Parsed, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let object = value
        .as_object()
        .ok_or("the top level is not a JSON object")?;
    let (mut providers, mut skipped) = (Providers::new(), Vec::new());
    for (name, url) in object {
        let Some(url) = url.as_str() else {
            skipped.push(format!("{name:?}: the value is not a string"));
            continue;
        };
        match (ProviderName::parse(name), ProviderUrl::parse(url)) {
            (Ok(n), Ok(u)) => {
                providers.insert(n.to_string(), u.to_string());
            }
            (Err(e), _) | (_, Err(e)) => skipped.push(format!("{name:?}: {e}")),
        }
    }
    Ok((providers, skipped))
}

/// Load the list from `path`; `now` (seconds since the epoch) names backup files.
pub fn load(path: &Path, uid: u32, now: u64) -> Loaded {
    let fallback = |notes| Loaded {
        providers: builtin_providers(),
        notes,
    };
    let bytes = match read_private_file(path, uid, MAX_FILE_BYTES) {
        Ok(None) => return fallback(Vec::new()),
        Ok(Some(b)) => b,
        Err(e) => {
            let mut notes = vec![format!("provider list not used: {e}")];
            match aside(path, "untrusted", now) {
                Ok(kept) => notes.push(format!("kept as {}", kept.display())),
                Err(err) => notes.push(format!("could not move it aside: {err}")),
            }
            notes.push("using the built-in providers".to_owned());
            return fallback(notes);
        }
    };
    match parse(&bytes) {
        Err(why) => {
            let mut notes = vec![format!("provider list is not valid ({why})")];
            match aside(path, "corrupt", now) {
                Ok(kept) => notes.push(format!("kept as {}", kept.display())),
                Err(err) => notes.push(format!("could not move it aside: {err}")),
            }
            notes.push("using the built-in providers".to_owned());
            fallback(notes)
        }
        Ok((providers, skipped)) => {
            let mut notes = Vec::new();
            if !skipped.is_empty() {
                notes.push(format!(
                    "ignored {} unusable provider(s): {}",
                    skipped.len(),
                    skipped.join("; ")
                ));
                let copy = path.with_file_name(format!(
                    "{}.dropped-{now}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
                match write_private_atomic(&copy, &bytes, false) {
                    Ok(()) => {
                        notes.push(format!("the original file is kept as {}", copy.display()));
                    }
                    Err(err) => notes.push(format!("could not keep a copy of the original: {err}")),
                }
            }
            if providers.is_empty() {
                notes.push("no usable providers: using the built-in providers".to_owned());
                return fallback(notes);
            }
            Loaded { providers, notes }
        }
    }
}

/// Write the list atomically as a `0600` file.
pub fn save(path: &Path, providers: &Providers) -> Result<(), PrivateError> {
    let mut text = serde_json::to_string_pretty(providers)
        .map_err(|e| PrivateError::Io(path.to_owned(), io::Error::other(e)))?;
    text.push('\n');
    write_private_atomic(path, text.as_bytes(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_wrapper_platform::runtime::current_uid;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lwd-prov-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn uid() -> u32 {
        current_uid().unwrap()
    }

    fn names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn missing_file_means_the_built_ins_and_no_fuss() {
        let d = dir();
        let l = load(&d.join("providers.json"), uid(), 1);
        assert_eq!(l.providers, builtin_providers());
        assert!(l.notes.is_empty());
    }

    #[test]
    fn save_then_load_round_trips_and_the_file_is_private() {
        let d = dir();
        let f = d.join("providers.json");
        let mut p = builtin_providers();
        p.insert("mistral".into(), "https://chat.mistral.ai".into());
        p.remove("claude");
        save(&f, &p).unwrap();
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let l = load(&f, uid(), 1);
        assert_eq!(l.providers, p);
        assert!(l.notes.is_empty());
        assert_eq!(names(&d), ["providers.json"]);
    }

    #[test]
    fn a_corrupt_file_is_kept_and_the_built_ins_take_over() {
        for garbage in [
            "{ \"mine\": \"https://mine.example\", this is not json",
            "[1,2,3]",
            "\"text\"",
            "",
        ] {
            let d = dir();
            let f = d.join("providers.json");
            fs::write(&f, garbage).unwrap();
            let l = load(&f, uid(), 1234);
            assert_eq!(l.providers, builtin_providers(), "{garbage:?}");
            assert_eq!(names(&d), ["providers.json.corrupt-1234"], "{garbage:?}");
            assert_eq!(
                fs::read_to_string(d.join("providers.json.corrupt-1234")).unwrap(),
                garbage
            );
            assert!(
                l.notes.iter().any(|n| n.contains("corrupt-1234")),
                "{:?}",
                l.notes
            );
        }
    }

    #[test]
    fn entries_that_are_not_acceptable_are_dropped_but_the_original_is_kept() {
        let d = dir();
        let f = d.join("providers.json");
        let text = r#"{"good":"https://good.example","lan":"http://192.168.1.10:3000","bad name":"https://x.example","num":5,"cred":"https://u:p@x.example"}"#;
        fs::write(&f, text).unwrap();
        let l = load(&f, uid(), 7);
        assert_eq!(l.providers.keys().collect::<Vec<_>>(), ["good"]);
        assert_eq!(names(&d), ["providers.json", "providers.json.dropped-7"]);
        assert_eq!(
            fs::read_to_string(d.join("providers.json.dropped-7")).unwrap(),
            text
        );
        let note = l.notes.join(" | ");
        for must in ["lan", "bad name", "num", "cred", "providers.json.dropped-7"] {
            assert!(note.contains(must), "{must} missing from {note}");
        }
    }

    #[test]
    fn an_empty_list_falls_back_to_the_built_ins_without_touching_the_file() {
        let d = dir();
        let f = d.join("providers.json");
        fs::write(&f, "{}").unwrap();
        let l = load(&f, uid(), 1);
        assert_eq!(l.providers, builtin_providers());
        assert_eq!(names(&d), ["providers.json"]);
    }

    #[test]
    fn untrusted_files_are_moved_aside_not_read() {
        let d = dir();
        let f = d.join("providers.json");
        fs::write(&f, r#"{"evil":"https://evil.example"}"#).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o666)).unwrap();
        let l = load(&f, uid(), 9);
        assert_eq!(
            l.providers,
            builtin_providers(),
            "a world-writable list must not be believed"
        );
        assert_eq!(names(&d), ["providers.json.untrusted-9"]);

        let d = dir();
        let target = d.join("elsewhere.json");
        fs::write(&target, r#"{"evil":"https://evil.example"}"#).unwrap();
        symlink(&target, d.join("providers.json")).unwrap();
        let l = load(&d.join("providers.json"), uid(), 9);
        assert_eq!(l.providers, builtin_providers());
        assert!(target.exists(), "the link target is not ours to move");

        let d = dir();
        fs::write(d.join("providers.json"), vec![b' '; 70_000]).unwrap();
        assert_eq!(
            load(&d.join("providers.json"), uid(), 1).providers,
            builtin_providers()
        );
        assert_eq!(names(&d), ["providers.json.untrusted-1"]);
    }

    #[test]
    fn backup_names_never_collide() {
        let d = dir();
        let f = d.join("providers.json");
        for i in 0..3 {
            fs::write(&f, format!("garbage {i}")).unwrap();
            load(&f, uid(), 5);
        }
        assert_eq!(
            names(&d),
            [
                "providers.json.corrupt-5",
                "providers.json.corrupt-5-1",
                "providers.json.corrupt-5-2"
            ]
        );
    }
}
