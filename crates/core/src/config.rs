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
}
