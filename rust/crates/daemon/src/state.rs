//! The state file other programs watch (a Quickshell bar, for instance).
//!
//! `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].json`, rewritten atomically so a reader never sees half a
//! file, `0600`, and only when something actually changed.

use std::path::PathBuf;

use llm_wrapper_platform::private::{write_private_atomic, PrivateError};
use serde_json::json;

use crate::core::Providers;

/// What the file says.
pub struct Snapshot<'a> {
    /// `--instance` name, if any.
    pub instance: Option<&'a str>,
    /// Active provider name (or URL for an ad-hoc one).
    pub active: &'a str,
    /// Its URL.
    pub url: &'a str,
    /// Whether the window is visible.
    pub visible: bool,
    /// The provider list.
    pub providers: &'a Providers,
}

/// Writer for one state file.
pub struct StateFile {
    path: PathBuf,
    last: Option<String>,
}

impl StateFile {
    /// A writer for `path`; nothing is written yet.
    pub fn new(path: PathBuf) -> Self {
        Self { path, last: None }
    }

    /// Write the file unless it already says exactly this. Returns whether it wrote.
    pub fn write(&mut self, s: &Snapshot<'_>) -> Result<bool, PrivateError> {
        let text = serde_json::to_string_pretty(&json!({
            "instance": s.instance,
            "active": s.active,
            "url": s.url,
            "visible": s.visible,
            "providers": s.providers,
        }))
        .map_err(|e| PrivateError::Io(self.path.clone(), std::io::Error::other(e)))?;
        if self.last.as_deref() == Some(text.as_str()) {
            return Ok(false);
        }
        write_private_atomic(&self.path, text.as_bytes(), false)?;
        self.last = Some(text);
        Ok(true)
    }

    /// Delete the file (at shutdown).
    pub fn remove(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::builtin_providers;
    use std::os::unix::fs::PermissionsExt;

    fn snap(p: &Providers, visible: bool) -> Snapshot<'_> {
        Snapshot {
            instance: None,
            active: "chatgpt",
            url: "https://chatgpt.com",
            visible,
            providers: p,
        }
    }

    #[test]
    fn schema_privacy_and_coalescing() {
        let dir = std::env::temp_dir().join(format!("lwd-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("llm-wrapper.json");
        let mut f = StateFile::new(path.clone());
        let providers = builtin_providers();

        assert!(f.write(&snap(&providers, false)).unwrap());
        assert!(
            !f.write(&snap(&providers, false)).unwrap(),
            "an identical state is not rewritten"
        );
        assert!(f.write(&snap(&providers, true)).unwrap());

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["active", "instance", "providers", "url", "visible"]);
        assert_eq!(
            (
                v["instance"].is_null(),
                v["visible"].as_bool(),
                v["url"].as_str()
            ),
            (true, Some(true), Some("https://chatgpt.com"))
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary file may stay"
        );

        f.remove();
        assert!(!path.exists());
        assert!(
            f.write(&snap(&providers, true)).unwrap(),
            "after removal the next write happens again"
        );
    }
}
