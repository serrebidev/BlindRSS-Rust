//! Over-the-air UI translation updates — port of Python
//! core/translation_updates.py. Fetches .po catalogs from the
//! repo's main branch with ETag conditionals and installs them
//! into <data_dir>/locale, which the app's catalog loader prefers
//! over the bundled tree. Divergence from Python: the source repo
//! is BlindRSS-Rust (where this app's bundled catalogs live), and
//! catalogs install as .po (the Rust loader parses .po directly;
//! Python compiles to .mo).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CFG_ENABLED: &str = "translation_auto_update";
pub const CFG_FREQUENCY: &str = "translation_update_frequency";
pub const CFG_LAST_CHECK: &str = "translation_update_last_check";
pub const DEFAULT_FREQUENCY: &str = "daily";
pub const STATE_FILENAME: &str = "translation_updates.json";
/// A catalog parsing to fewer messages is treated as damaged.
pub const MIN_SANE_MESSAGES: usize = 50;

const CATALOG_URL_TEMPLATE: &str = "https://raw.githubusercontent.com/serrebidev/BlindRSS-Rust/main/locale/{lang}/LC_MESSAGES/blindrss.po";

pub fn frequency_seconds(frequency: &str) -> Option<i64> {
    match frequency.to_lowercase().as_str() {
        // Explicit: "never" has no interval (is_due short-
        // circuits it before this fallback is consulted).
        "never" => None,
        "startup" => Some(0),
        "ten_minutes" => Some(600),
        "daily" => Some(86_400),
        "weekly" => Some(604_800),
        "monthly" => Some(2_592_000),
        _ => None,
    }
}

pub fn override_root(data_dir: &Path) -> PathBuf {
    data_dir.join("locale")
}

/// Where one language's downloaded override catalog lives
/// (Python `catalog_path`).
pub fn catalog_path(data_dir: &Path, language: &str) -> PathBuf {
    override_root(data_dir)
        .join(language)
        .join("LC_MESSAGES")
        .join("blindrss.po")
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILENAME)
}

pub fn load_state(data_dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(state_path(data_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn save_state(data_dir: &Path, state: &serde_json::Map<String, serde_json::Value>) {
    let _ = std::fs::create_dir_all(data_dir);
    if let Ok(text) = serde_json::to_string_pretty(&serde_json::Value::Object(state.clone())) {
        let _ = std::fs::write(state_path(data_dir), text);
    }
}

/// False when the override tree would BE one of the bundled trees
/// (Python's overrides_available guard).
pub fn overrides_available(data_dir: &Path, bundled_roots: &[PathBuf]) -> bool {
    let root = override_root(data_dir);
    let canon = std::fs::canonicalize(&root).unwrap_or(root.clone());
    for bundled in bundled_roots {
        let b = std::fs::canonicalize(bundled).unwrap_or_else(|_| bundled.clone());
        if canon == b {
            return false;
        }
    }
    true
}

/// Outcome of [`prepare_overrides_for_app_version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareOutcome {
    /// The recorded version already matches (or there is
    /// nothing to prepare): the override tree may be used
    /// as-is.
    Current,
    /// A version transition was handled: stale overrides were
    /// removed and the new version recorded.
    Reset,
    /// Removing the stale override tree failed. The new
    /// version was NOT recorded, so the reset retries on the
    /// next launch, and the caller must fail closed to the
    /// bundled catalogs for this run — Python raises out of
    /// the cleanup, and `core/i18n.py`'s
    /// `override_locale_dir` answers "" (bundled only) when
    /// prepare raises. Recording the version anyway would
    /// strand the stale override permanently: state claims
    /// the reset happened while old catalogs still shadow
    /// the bundled ones.
    CleanupFailed,
}

/// Make bundled catalogs authoritative after a version change:
/// drop downloaded overrides + etags once per app version.
pub fn prepare_overrides_for_app_version(
    data_dir: &Path,
    bundled_roots: &[PathBuf],
    app_version: &str,
) -> PrepareOutcome {
    let current = app_version.trim();
    if current.is_empty() || !overrides_available(data_dir, bundled_roots) {
        return PrepareOutcome::Current;
    }
    let mut state = load_state(data_dir);
    let recorded = state
        .get("app_version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if recorded == current {
        return PrepareOutcome::Current;
    }
    let root = override_root(data_dir);
    if root.exists() && std::fs::remove_dir_all(&root).is_err() {
        // Fail closed: no state write at all, so the recorded
        // version, etags and last_check all survive for the
        // retry, exactly as Python leaves them when rmtree
        // raises before save_state.
        return PrepareOutcome::CleanupFailed;
    }
    state.insert("app_version".to_string(), serde_json::json!(current));
    if let Some(obj) = state.get_mut("etags") {
        *obj = serde_json::json!({});
    }
    state.remove("last_check");
    save_state(data_dir, &state);
    PrepareOutcome::Reset
}

/// Languages that already have a downloaded override catalog.
pub fn installed_languages(data_dir: &Path) -> Vec<String> {
    let root = override_root(data_dir);
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let po = entry.path().join("LC_MESSAGES").join("blindrss.po");
            if po.is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                found.push(name.to_string());
            }
        }
    }
    found.sort();
    found
}

pub fn last_check_time(data_dir: &Path) -> i64 {
    load_state(data_dir)
        .get("last_check")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// True when the configured interval since the last check elapsed.
/// "never" (Brandon's order, 2026-11-07: the General tab's
/// update-check frequency gains a Disabled option) is never due —
/// automatic checks stop; manual checks bypass is_due entirely.
pub fn is_due(data_dir: &Path, frequency: &str, now: i64) -> bool {
    if frequency.eq_ignore_ascii_case("never") {
        return false;
    }
    let interval = frequency_seconds(frequency)
        .unwrap_or_else(|| frequency_seconds(DEFAULT_FREQUENCY).unwrap_or(86_400));
    let last = last_check_time(data_dir);
    if last == 0 {
        return true;
    }
    now - last >= interval
}

/// Remove every downloaded catalog (Settings' reset path).
pub fn clear_overrides(data_dir: &Path, bundled_roots: &[PathBuf]) {
    if !overrides_available(data_dir, bundled_roots) {
        return;
    }
    let root = override_root(data_dir);
    if root.exists() {
        let _ = std::fs::remove_dir_all(&root);
    }
    let mut state = load_state(data_dir);
    state.remove("etags");
    save_state(data_dir, &state);
}

pub enum FetchOutcome {
    Updated { text: String, etag: String },
    Unchanged,
    Missing,
    Failed(String),
}

/// The network half, injected so tests never touch the wire.
pub trait CatalogFetch {
    fn fetch(&self, url: &str, etag: &str) -> FetchOutcome;
}

/// ureq-backed fetcher.
pub struct HttpFetch;

impl CatalogFetch for HttpFetch {
    fn fetch(&self, url: &str, etag: &str) -> FetchOutcome {
        let agent = crate::http::agent();
        let mut req = agent.get(url);
        if !etag.is_empty() {
            req = req.header("If-None-Match", etag);
        }
        match req.call() {
            Ok(mut resp) => {
                if resp.status().as_u16() == 304 {
                    return FetchOutcome::Unchanged;
                }
                let new_etag = resp
                    .headers()
                    .get("ETag")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                match resp.body_mut().read_to_string() {
                    Ok(text) => FetchOutcome::Updated {
                        text,
                        etag: new_etag,
                    },
                    Err(e) => FetchOutcome::Failed(e.to_string()),
                }
            }
            Err(ureq::Error::StatusCode(304)) => FetchOutcome::Unchanged,
            Err(ureq::Error::StatusCode(404)) => FetchOutcome::Missing,
            Err(e) => FetchOutcome::Failed(e.to_string()),
        }
    }
}

#[derive(Debug, Default)]
pub struct UpdateResult {
    pub updated: Vec<String>,
    pub checked: usize,
    pub error: Option<String>,
}

impl UpdateResult {
    pub fn changed(&self) -> bool {
        !self.updated.is_empty()
    }
}

fn install_catalog(data_dir: &Path, language: &str, po_text: &str) -> bool {
    let catalog = crate::i18n::Catalog::parse(po_text);
    if catalog.len() < MIN_SANE_MESSAGES {
        return false;
    }
    let dest = catalog_path(data_dir, language);
    let dir = dest.parent().expect("catalog path has a parent");
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tmp = dir.join(".blindrss.po.tmp");
    if std::fs::write(&tmp, po_text).is_err() {
        return false;
    }
    std::fs::rename(&tmp, &dest).is_ok()
}

/// Fetch and install newer catalogs for `languages`. Never errors
/// out: transport problems land in UpdateResult.error. With
/// `force`, stored validators are ignored and every catalog is
/// fetched unconditionally (Python's Settings "Check for
/// Updates" path); the scheduled checks pass `force: false` and
/// stay conditional.
pub fn check_and_update(
    data_dir: &Path,
    bundled_roots: &[PathBuf],
    languages: &[String],
    fetcher: &dyn CatalogFetch,
    now: i64,
    force: bool,
) -> UpdateResult {
    let mut result = UpdateResult::default();
    if !overrides_available(data_dir, bundled_roots) {
        return result;
    }
    let mut wanted: Vec<String> = Vec::new();
    for code in languages {
        let code = code.trim().to_string();
        if code.is_empty()
            || matches!(code.to_lowercase().as_str(), "en" | "c" | "posix")
            || wanted.contains(&code)
        {
            continue;
        }
        wanted.push(code);
    }
    if wanted.is_empty() {
        return result;
    }
    let mut state = load_state(data_dir);
    let mut etags: BTreeMap<String, String> = state
        .get("etags")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    for lang in &wanted {
        result.checked += 1;
        // Python: a forced check sends no validator at all.
        let mut etag = if force {
            String::new()
        } else {
            etags.get(lang).cloned().unwrap_or_default()
        };
        // An override recorded in state but missing on disk
        // must be re-fetched even when the ETag says
        // "unchanged": sending the stale validator would let
        // a 304 confirm a file that does not exist and strand
        // the language with no catalog at all.
        if !etag.is_empty() && !catalog_path(data_dir, lang).is_file() {
            etag.clear();
        }
        let url = CATALOG_URL_TEMPLATE.replace("{lang}", lang);
        match fetcher.fetch(&url, &etag) {
            FetchOutcome::Updated {
                text,
                etag: new_etag,
            } => {
                if install_catalog(data_dir, lang, &text) {
                    result.updated.push(lang.clone());
                    if !new_etag.is_empty() {
                        etags.insert(lang.clone(), new_etag);
                    }
                }
            }
            FetchOutcome::Unchanged => {}
            FetchOutcome::Missing => {
                etags.remove(lang);
            }
            FetchOutcome::Failed(e) => {
                if result.error.is_none() {
                    result.error = Some(e);
                }
            }
        }
    }
    state.insert(
        "etags".to_string(),
        serde_json::Value::Object(
            etags
                .into_iter()
                .map(|(k, v)| (k, serde_json::json!(v)))
                .collect(),
        ),
    );
    state.insert("last_check".to_string(), serde_json::json!(now));
    save_state(data_dir, &state);
    result
}
