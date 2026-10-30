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
                let raw = serde_json::from_str(&text).unwrap_or(Value::Null);
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

    /// Persist to disk in the Python format (pretty JSON).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.raw).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, text)
    }
}
