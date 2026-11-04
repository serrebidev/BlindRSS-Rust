//! Reader for the existing Python `config.json`.
//!
//! The Python app owns the format (core/config.py). This reader is
//! deliberately permissive: unknown keys are preserved in the raw value,
//! missing keys fall back to the caller's default, mirroring how the
//! Python loader merges defaults.

use serde_json::Value;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Write a file by replacing a sibling temporary file, mirroring
/// Python's `ConfigManager.save_config` (`core/config.py`): the
/// text is fully written, flushed and fsynced into
/// `<path>.tmp`, then `os.replace` swaps it over the live file, so
/// a crash or failed write leaves the complete old file in place.
/// The temp file is removed on any failure. Never truncates the
/// live file directly.
pub fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Sibling temporary path used by [`atomic_write`], matching
/// Python's `<config_path>.tmp`.
fn temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

/// Pure two-location choice behind [`resolve_config_path`],
/// mirroring Python's `_resolve_config_path` (`core/config.py`):
/// installed Windows builds always use user data; when both
/// locations exist the newer config wins (a tie goes to user
/// data, Python's `>=`); a single existing config wins; with
/// neither, the platform default (`default_is_user`) wins.
pub fn choose_config_path(
    user_config: &Path,
    app_config: &Path,
    user_mtime: Option<SystemTime>,
    app_mtime: Option<SystemTime>,
    windows_installed: bool,
    default_is_user: bool,
) -> PathBuf {
    if windows_installed {
        return user_config.to_path_buf();
    }
    match (user_mtime, app_mtime) {
        (Some(user), Some(app)) => {
            if user >= app {
                user_config.to_path_buf()
            } else {
                app_config.to_path_buf()
            }
        }
        (Some(_), None) => user_config.to_path_buf(),
        (None, Some(_)) => app_config.to_path_buf(),
        (None, None) => {
            if default_is_user {
                user_config.to_path_buf()
            } else {
                app_config.to_path_buf()
            }
        }
    }
}

/// Filesystem-backed [`choose_config_path`]: mtimes are read from
/// the two candidate config files (a missing file counts as
/// absent). `default_is_user` is the platform fresh-install
/// default: true for installed Windows builds and macOS, false
/// elsewhere (Python's `_default_config_location`).
pub fn resolve_config_path(
    user_config: &Path,
    app_config: &Path,
    windows_installed: bool,
    default_is_user: bool,
) -> PathBuf {
    let mtime = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
    choose_config_path(
        user_config,
        app_config,
        mtime(user_config),
        mtime(app_config),
        windows_installed,
        default_is_user,
    )
}

/// Copy a legacy data file into a location only when the target
/// does not already exist, via a sibling temporary file and
/// replace — Python's `_copy_file_if_missing` (`core/config.py`).
/// Returns whether the target exists afterwards.
pub fn copy_file_if_missing(source: &Path, target: &Path) -> bool {
    if !source.is_file() || target.exists() {
        return target.exists();
    }
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    let temp = PathBuf::from(format!(
        "{}.migrating-{}",
        target.display(),
        std::process::id()
    ));
    let result = std::fs::copy(source, &temp).and_then(|_| std::fs::rename(&temp, target));
    match result {
        Ok(()) => true,
        Err(_) => {
            let _ = std::fs::remove_file(&temp);
            false
        }
    }
}

/// A remembered main-window view in the Rust tree's internal
/// terms, decoded from the shared `last_selected_feed` format.
/// `view_id` is `None` for the All Articles view, `cat:<path>`
/// for a category, `smart:<id>` for a smart folder, a plain feed
/// id otherwise, or `favorites:all` / `deleted:all` for Python's
/// special views (the Rust tree has no node for those; see the
/// app-side mapping). `read_filter` is 0 = all, 1 = unread,
/// 2 = read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RememberedView {
    pub read_filter: u8,
    pub view_id: Option<String>,
}

/// Decode a Python- or Rust-written `last_selected_feed` value
/// (`gui/mainframe.py` restore path): an optional `unread:` /
/// `read:` filter prefix, then `all`, `category:<path>`
/// (Rust's legacy `cat:<path>` is accepted too), `favorites:all`,
/// `deleted:all`, `smart:<id>` or a feed id.
pub fn decode_remembered_view(raw: &str) -> RememberedView {
    let (rest, read_filter) = if let Some(rest) = raw.strip_prefix("unread:") {
        (rest, 1)
    } else if let Some(rest) = raw.strip_prefix("read:") {
        (rest, 2)
    } else {
        (raw, 0)
    };
    let view_id = match rest {
        "" | "all" => None,
        _ => {
            let path = rest.strip_prefix("category:").unwrap_or(rest);
            Some(path.to_string())
        }
    };
    // `category:` maps to the Rust tree's `cat:` id; every other
    // id (including legacy `cat:`, `favorites:all`, `deleted:all`,
    // `smart:`, feed ids) already is a Rust tree id.
    let view_id = view_id.map(|id| {
        if rest.starts_with("category:") {
            format!("cat:{id}")
        } else {
            id
        }
    });
    RememberedView {
        read_filter,
        view_id,
    }
}

/// Encode a Rust tree selection to the shared format Python
/// writes (`_get_feed_id_from_tree_item` +
/// `_wrap_view_id_with_filter`): `cat:<path>` becomes
/// `category:<path>`, an empty selection becomes `all`, and an
/// `unread:`/`read:` prefix is applied — never to smart folders
/// or the Deleted view, which carry their own semantics.
pub fn encode_remembered_view(view_id: Option<&str>, read_filter: u8) -> String {
    let base = match view_id {
        None | Some("") => "all".to_string(),
        Some(id) => id
            .strip_prefix("cat:")
            .map(|p| format!("category:{p}"))
            .unwrap_or_else(|| id.to_string()),
    };
    if (read_filter == 1 || read_filter == 2)
        && !base.starts_with("smart:")
        && !base.starts_with("deleted:")
    {
        let prefix = if read_filter == 1 { "unread" } else { "read" };
        format!("{prefix}:{base}")
    } else {
        base
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    raw: Value,
    /// The file this config was loaded from, when it was.
    /// Lets long-lived credential state (OAuth token rotation,
    /// Audit 8 D07) write itself back without every call site
    /// threading the path through; `Config::default()` and
    /// hand-built configs have none and skip write-back.
    source_path: Option<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                // A UTF-8 BOM (Notepad, some editors) is not JSON; strip
                // it so a BOM'd config is read, not discarded.
                let text = text.trim_start_matches('\u{feff}');
                let raw = match serde_json::from_str::<Value>(text) {
                    Ok(v) if v.is_object() => v,
                    _ => {
                        // A config that parses to nothing (or to a
                        // non-object) must not be treated as empty: the
                        // next save would silently overwrite the user's
                        // settings with nothing. Keep the file beside
                        // itself so it can be recovered by hand.
                        let _ = std::fs::copy(path, unreadable_path(path));
                        Value::Null
                    }
                };
                Ok(Self {
                    raw,
                    source_path: Some(path.to_path_buf()),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                source_path: Some(path.to_path_buf()),
                ..Self::default()
            }),
            Err(e) => Err(e),
        }
    }

    /// The file this config was loaded from, if it was.
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// Save back to the file this config was loaded from.
    /// Fails when the config has no source file, or when it
    /// holds no object shape (an unreadable load must never be
    /// written back over the user's file).
    pub fn save_to_source(&self) -> std::io::Result<()> {
        let Some(path) = &self.source_path else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "config has no source file",
            ));
        };
        if !self.raw.is_object() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "config has no object shape to save",
            ));
        }
        self.save(path)
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.raw
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    pub fn get_i64(&self, key: &str, default: i64) -> i64 {
        self.raw.get(key).and_then(Value::as_i64).unwrap_or(default)
    }

    pub fn get_str<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.raw.get(key).and_then(Value::as_str).unwrap_or(default)
    }

    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// Mutable access for the settings writer; the object shape is
    /// created on demand.
    pub fn raw_mut(&mut self) -> &mut Value {
        if !self.raw.is_object() {
            self.raw = Value::Object(serde_json::Map::new());
        }
        &mut self.raw
    }

    /// Set a dotted path (`providers.miniflux.url`) to a JSON value.
    pub fn set_path(&mut self, path: &str, value: Value) {
        let parts: Vec<&str> = path.split('.').collect();
        let mut cur = self.raw_mut();
        for part in &parts[..parts.len() - 1] {
            if !cur.get(*part).is_some_and(|v| v.is_object()) {
                cur.as_object_mut()
                    .unwrap()
                    .insert(part.to_string(), Value::Object(serde_json::Map::new()));
            }
            cur = cur.get_mut(*part).unwrap();
        }
        cur.as_object_mut()
            .unwrap()
            .insert(parts[parts.len() - 1].to_string(), value);
    }

    /// Persist to disk in the Python format (pretty JSON). A config
    /// with no object shape (fresh install, unreadable load) is
    /// written as `{}` so Python can always read it back. The write
    /// is atomic: [`atomic_write`] fully writes and fsyncs a
    /// sibling temp file, then replaces — a failed write leaves
    /// the complete old config in place (Audit 8 S02).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let raw = if self.raw.is_object() {
            self.raw.clone()
        } else {
            Value::Object(serde_json::Map::new())
        };
        let text = serde_json::to_string_pretty(&raw).unwrap_or_else(|_| "{}".to_string());
        atomic_write(path, &text)
    }

    /// Move this config to another storage location, mirroring
    /// Python's `ConfigManager.change_data_location`: sets the
    /// `data_location` key, atomically writes the config at the
    /// new location and removes the old file. The database file is
    /// not moved here — it is handled on next startup, as in
    /// Python. Installed Windows builds are restricted to user
    /// data. On any write failure the old config file and the
    /// in-memory `data_location` are left untouched and an error
    /// is returned (Audit 8 S11).
    pub fn change_data_location(
        &mut self,
        old_path: &Path,
        new_location: &str,
        user_config: &Path,
        app_config: &Path,
        windows_installed: bool,
    ) -> Result<PathBuf, String> {
        let target = if new_location == "user_data" {
            "user_data"
        } else {
            "app_folder"
        };
        if windows_installed && target != "user_data" {
            return Err("Installed Windows builds store data in the User Data Folder.".to_string());
        }
        let new_path = if target == "user_data" {
            user_config.to_path_buf()
        } else {
            app_config.to_path_buf()
        };
        let previous = self.get_str("data_location", "").to_string();
        self.set_path("data_location", Value::String(target.to_string()));
        match self.save(&new_path) {
            Ok(()) => {}
            Err(e) => {
                self.set_path("data_location", Value::String(previous));
                return Err(format!("Could not write config at new location: {e}"));
            }
        }
        // Remove the old file so future resolution is
        // unambiguous; a failure to remove is not fatal in
        // Python (the newer written file wins resolution).
        if new_path != old_path {
            let _ = std::fs::remove_file(old_path);
        }
        self.source_path = Some(new_path.clone());
        Ok(new_path)
    }
}

/// Where an unreadable config is kept: `config.json` ->
/// `config.unreadable.json` beside it.
fn unreadable_path(path: &Path) -> std::path::PathBuf {
    path.with_file_name(format!(
        "{}.unreadable.json",
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("config")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("blindrss_config_tests");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join(name);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(unreadable_path(&p));
        p
    }

    #[test]
    fn bom_config_loads() {
        let path = scratch("bom.json");
        std::fs::write(&path, "\u{feff}{\"volume\": 42}").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.get_i64("volume", 0), 42);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unreadable_config_is_kept_not_lost() {
        let path = scratch("broken.json");
        std::fs::write(&path, "{\"volume\": 42,").unwrap();
        let mut cfg = Config::load(&path).unwrap();
        cfg.set_path("volume", serde_json::json!(10));
        cfg.save(&path).unwrap();
        // The save is a fresh config, but the original is recoverable.
        let kept = std::fs::read_to_string(unreadable_path(&path)).unwrap();
        assert_eq!(kept, "{\"volume\": 42,");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(unreadable_path(&path));
    }

    #[test]
    fn non_object_config_is_not_written_back() {
        let path = scratch("array.json");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        let cfg = Config::load(&path).unwrap();
        cfg.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "{}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(unreadable_path(&path));
    }
}
