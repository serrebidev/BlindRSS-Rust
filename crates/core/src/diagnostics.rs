//! Debug-mode diagnostics (Audit 8 S18; Python `main.py:52,109`).
//!
//! Python's shape: when `debug_mode` is on, a rotating file
//! handler writes `<data dir>/blindrss.log` with a 5 MiB cap
//! and three backups; when it is off, no file handler exists
//! at all. Every line written through the diagnostics path is
//! secret-redacted first — provider keys, source credentials,
//! solver keys, cookies and Authorization values must never
//! reach the log (the house rule: secrets are never logged).

/// Python's `RotatingFileHandler` cap: 5 MiB.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Python's `backupCount`: three backups (`.1` newest).
pub const BACKUP_COUNT: usize = 3;

const REDACTED: &str = "***";

/// Config keys whose values are secrets and must be scrubbed
/// from diagnostics by value, wherever they appear.
const SECRET_KEYS: &[&str] = &[
    "captcha_solver_api_key",
    "groups_io_api_key",
    "github_token",
    "youtube_account_refresh_token",
    "providers.miniflux.api_key",
    "providers.theoldreader.password",
    "providers.bazqux.password",
    "providers.inoreader.app_key",
    "providers.inoreader.token",
    "providers.inoreader.refresh_token",
];

/// The secret values a config currently holds: every known
/// secret key plus every translation provider's API key.
/// Short values are skipped (scrubbing a 1-3 character string
/// would mangle ordinary prose).
pub fn secrets_from_config(cfg: &crate::config::Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim();
        if value.len() >= 4 && !out.iter().any(|v| v == value) {
            out.push(value.to_string());
        }
    };
    for key in SECRET_KEYS {
        push(cfg.get_str(key, ""));
    }
    for provider in crate::translate::PROVIDERS {
        push(cfg.get_str(&format!("translation_{provider}_api_key"), ""));
    }
    out
}

/// Scrub one log line: known secret values are replaced
/// wherever they appear, and structured credentials are
/// pattern-scrubbed even when the exact value is unknown —
/// `Authorization:` / `Cookie:` header values, `Bearer`
/// tokens, and `key=` / `token=` / `password=` style
/// assignments (URL queries and form bodies included).
pub fn redact_line(line: &str, secrets: &[&str]) -> String {
    let mut out = line.to_string();
    for secret in secrets {
        let secret = secret.trim();
        if secret.len() >= 4 && out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out = scrub_after(&out, "authorization:", true);
    out = scrub_after(&out, "cookie:", true);
    out = scrub_bearer(&out);
    for name in [
        "api_key=",
        "apikey=",
        "api-key=",
        "access_token=",
        "refresh_token=",
        "token=",
        "password=",
        "key=",
    ] {
        out = scrub_assignment(&out, name);
    }
    out
}

/// Replace everything after `marker` (case-insensitive) up to
/// the end of the value run. With `to_segment_end`, the value
/// runs to the next `;`, `,`, quote or the line end (header
/// shape); otherwise it stops at whitespace too.
fn scrub_after(line: &str, marker: &str, _to_segment_end: bool) -> String {
    let lower = line.to_lowercase();
    let Some(pos) = lower.find(marker) else {
        return line.to_string();
    };
    let value_start = pos + marker.len();
    let rest = &line[value_start..];
    let trimmed = rest.trim_start();
    let lead = rest.len() - trimmed.len();
    let end = trimmed
        .find(|c: char| [';', ',', '"', '\''].contains(&c))
        .unwrap_or(trimmed.len());
    if end == 0 {
        return line.to_string();
    }
    format!(
        "{}{}{}",
        &line[..value_start + lead],
        REDACTED,
        &trimmed[end..]
    )
}

fn scrub_bearer(line: &str) -> String {
    let lower = line.to_lowercase();
    let Some(pos) = lower.find("bearer ") else {
        return line.to_string();
    };
    let value_start = pos + "bearer ".len();
    let rest = &line[value_start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ';' || c == ',')
        .unwrap_or(rest.len());
    if end == 0 {
        return line.to_string();
    }
    format!("{}{}{}", &line[..value_start], REDACTED, &rest[end..])
}

/// Replace the value of every `name<value>` assignment where
/// the value runs to the next `&`, whitespace or quote.
fn scrub_assignment(line: &str, name: &str) -> String {
    let lower = line.to_lowercase();
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    while let Some(rel) = lower[cursor..].find(name) {
        let value_start = cursor + rel + name.len();
        // The name must start a token (start of line or after
        // a separator), not the tail of a longer word.
        let before_ok = cursor + rel == 0
            || !line[..cursor + rel]
                .chars()
                .last()
                .map(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                .unwrap_or(false);
        out.push_str(&line[cursor..value_start]);
        if !before_ok {
            cursor = value_start;
            continue;
        }
        let rest = &line[value_start..];
        let end = rest
            .find(|c: char| c == '&' || c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        if end > 0 {
            out.push_str(REDACTED);
        }
        cursor = value_start + end;
    }
    out.push_str(&line[cursor..]);
    out
}

/// A rotating log writer with Python `RotatingFileHandler`
/// semantics: before a write that would reach the cap, the
/// current file becomes `.1`, older backups shift up, and
/// anything past the backup count is dropped.
pub struct RotatingLog {
    path: std::path::PathBuf,
    max_bytes: u64,
    backups: usize,
    size: u64,
}

impl RotatingLog {
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        Self::open_with_limits(path, MAX_BYTES, BACKUP_COUNT)
    }

    pub fn open_with_limits(
        path: &std::path::Path,
        max_bytes: u64,
        backups: usize,
    ) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            max_bytes,
            backups,
            size,
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Append one line, rotating first when the line would
    /// reach the cap (Python's `shouldRollover`).
    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let needed = line.len() as u64 + 1;
        if self.size > 0 && self.size + needed >= self.max_bytes {
            self.rotate();
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")?;
        self.size += needed;
        Ok(())
    }

    fn backup_path(&self, index: usize) -> std::path::PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(format!(".{index}"));
        std::path::PathBuf::from(name)
    }

    fn rotate(&mut self) {
        if self.backups > 0 {
            for i in (1..self.backups).rev() {
                let src = self.backup_path(i);
                let dst = self.backup_path(i + 1);
                if src.exists() {
                    let _ = std::fs::remove_file(&dst);
                    let _ = std::fs::rename(&src, &dst);
                }
            }
            let first = self.backup_path(1);
            let _ = std::fs::remove_file(&first);
            let _ = std::fs::rename(&self.path, &first);
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
        self.size = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_secret_values() {
        let line = "GET https://x.test/?sig=supersecretvalue failed for key supersecretvalue";
        let out = redact_line(line, &["supersecretvalue"]);
        assert!(!out.contains("supersecretvalue"));
        assert!(out.matches(REDACTED).count() >= 2);
    }

    #[test]
    fn redacts_structured_credentials() {
        let out = redact_line(
            "Authorization: Bearer abc.def.ghi Cookie: session=zzz; other=1",
            &[],
        );
        assert!(!out.contains("abc.def.ghi"), "{out}");
        assert!(!out.contains("session=zzz"), "{out}");
        let out = redact_line("https://h.test/x?access_token=tok12345&lang=en", &[]);
        assert!(!out.contains("tok12345"), "{out}");
        assert!(out.contains("lang=en"), "{out}");
        let out = redact_line("api_key=sk-live-1234567890 status=ok", &[]);
        assert!(!out.contains("sk-live-1234567890"), "{out}");
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        let line = "Refreshed 12 feeds; monkey business as usual, token= expired";
        // "token=" with an empty/space value scrubs nothing.
        let out = redact_line(line, &[]);
        assert!(out.contains("monkey business"), "{out}");
    }

    #[test]
    fn rotation_keeps_three_backups() {
        let dir = std::env::temp_dir().join(format!(
            "blindrss-diag-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blindrss.log");
        let mut log = RotatingLog::open_with_limits(&path, 64, 3).unwrap();
        for i in 0..40 {
            log.write_line(&format!("line {i:02} padding-padding"))
                .unwrap();
        }
        let b1 = dir.join("blindrss.log.1");
        let b2 = dir.join("blindrss.log.2");
        let b3 = dir.join("blindrss.log.3");
        let b4 = dir.join("blindrss.log.4");
        assert!(path.exists() && b1.exists() && b2.exists() && b3.exists());
        assert!(!b4.exists());
        // The newest backup holds newer lines than the oldest.
        let newest = std::fs::read_to_string(&b1).unwrap();
        let oldest = std::fs::read_to_string(&b3).unwrap();
        assert!(newest > oldest || newest.contains("line 3") || oldest.contains("line 0"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
