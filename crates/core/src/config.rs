//! Reader for the existing Python `config.json`.
//!
//! The Python app owns the format (core/config.py). This reader is
//! deliberately permissive: unknown keys are preserved in the raw value,
//! missing keys fall back to the caller's default, mirroring how the
//! Python loader merges defaults.

use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Config {
    raw: Value,
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
                Ok(Self { raw })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
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
    /// written as `{}` so Python can always read it back.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let raw = if self.raw.is_object() {
            self.raw.clone()
        } else {
            Value::Object(serde_json::Map::new())
        };
        let text = serde_json::to_string_pretty(&raw).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, text)
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
