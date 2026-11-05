//! Audit 8 Task 31 (O16): translation override lifecycle —
//! version-reset cleanup fails closed, absent catalogs clear
//! their ETags, and forced checks bypass validators, matching
//! Python core/translation_updates.py.

use blindrss_core::i18n::Catalog;
use blindrss_core::translation_updates as tu;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "blindrss-t31-{name}-{}-{nanos}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A catalog comfortably above MIN_SANE_MESSAGES.
fn big_po(msgid: &str, msgstr: &str) -> String {
    let mut out = String::new();
    for i in 0..60 {
        out.push_str(&format!(
            "msgid \"{msgid} {i}\"\nmsgstr \"{msgstr} {i}\"\n\n"
        ));
    }
    out
}

/// Solver-style conditional endpoint: a request carrying a
/// validator is answered Unchanged (what a 304 means); an
/// unconditional request gets the catalog body.
struct ConditionalFetch {
    calls: Mutex<Vec<String>>,
    text: String,
    etag: String,
}

impl ConditionalFetch {
    fn new(text: String, etag: &str) -> Self {
        ConditionalFetch {
            calls: Mutex::new(Vec::new()),
            text,
            etag: etag.to_string(),
        }
    }
    fn seen(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl tu::CatalogFetch for ConditionalFetch {
    fn fetch(&self, _url: &str, etag: &str) -> tu::FetchOutcome {
        self.calls.lock().unwrap().push(etag.to_string());
        if etag.is_empty() {
            tu::FetchOutcome::Updated {
                text: self.text.clone(),
                etag: self.etag.clone(),
            }
        } else {
            tu::FetchOutcome::Unchanged
        }
    }
}

fn seed_state(dir: &std::path::Path, version: Option<&str>, fr_etag: Option<&str>) {
    let mut state = serde_json::Map::new();
    if let Some(v) = version {
        state.insert("app_version".to_string(), serde_json::json!(v));
    }
    if let Some(e) = fr_etag {
        state.insert("etags".to_string(), serde_json::json!({ "fr": e }));
    }
    state.insert("last_check".to_string(), serde_json::json!(123));
    tu::save_state(dir, &state);
}

#[test]
fn failed_cleanup_does_not_record_version_and_retries() {
    let dir = scratch("cleanup");
    let bundled = vec![dir.join("bundled")];
    seed_state(&dir, Some("0.1.0"), Some("W/\"abc\""));
    // Obstruct the override root with a regular file so its
    // removal fails deterministically (remove_dir_all on a
    // non-directory errors on every platform).
    std::fs::write(tu::override_root(&dir), b"not a directory").unwrap();

    let outcome = tu::prepare_overrides_for_app_version(&dir, &bundled, "0.2.0");
    assert_eq!(outcome, tu::PrepareOutcome::CleanupFailed);
    // Nothing was recorded: the old version, the etags and
    // last_check all survive, so the reset retries next time
    // instead of stranding the stale state permanently.
    let state = tu::load_state(&dir);
    assert_eq!(
        state.get("app_version").and_then(|v| v.as_str()),
        Some("0.1.0")
    );
    assert_eq!(
        state
            .get("etags")
            .and_then(|v| v.get("fr"))
            .and_then(|v| v.as_str()),
        Some("W/\"abc\"")
    );
    assert_eq!(state.get("last_check").and_then(|v| v.as_i64()), Some(123));

    // Clear the obstruction and plant a real stale catalog:
    // the retried prepare now resets, records the version and
    // clears validators.
    std::fs::remove_file(tu::override_root(&dir)).unwrap();
    let fetch = ConditionalFetch::new(big_po("Hello", "Bonjour"), "W/\"abc\"");
    let langs = vec!["fr".to_string()];
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_000, false);
    assert_eq!(result.updated, langs);
    assert_eq!(tu::installed_languages(&dir), langs);

    let outcome = tu::prepare_overrides_for_app_version(&dir, &bundled, "0.2.0");
    assert_eq!(outcome, tu::PrepareOutcome::Reset);
    assert!(tu::installed_languages(&dir).is_empty());
    let state = tu::load_state(&dir);
    assert_eq!(
        state.get("app_version").and_then(|v| v.as_str()),
        Some("0.2.0")
    );
    assert!(
        state.get("etags").and_then(|v| v.get("fr")).is_none(),
        "reset clears stored etags"
    );
    assert!(
        state.get("last_check").is_none(),
        "reset makes the next check due"
    );

    // Same version again: nothing to do.
    assert_eq!(
        tu::prepare_overrides_for_app_version(&dir, &bundled, "0.2.0"),
        tu::PrepareOutcome::Current
    );
}

#[test]
fn absent_catalog_clears_etag_and_recovers() {
    let dir = scratch("etag");
    let bundled = vec![dir.join("bundled")];
    // ETag recorded in state, but the catalog file is gone:
    // sending the validator would let a 304 "confirm" a file
    // that does not exist.
    seed_state(&dir, None, Some("W/\"abc\""));
    let fetch = ConditionalFetch::new(big_po("Hello", "Bonjour"), "W/\"def\"");
    let langs = vec!["fr".to_string()];

    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_000, false);
    assert_eq!(fetch.seen(), [""]);
    assert_eq!(result.updated, langs);
    assert_eq!(tu::installed_languages(&dir), langs);
    assert!(tu::catalog_path(&dir, "fr").is_file());

    // With the file back on disk, the normal conditional path
    // still applies: the stored validator goes out and an
    // Unchanged answer installs nothing.
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_100, false);
    assert_eq!(fetch.seen(), ["", "W/\"def\""]);
    assert!(result.updated.is_empty());
}

#[test]
fn forced_check_ignores_validators() {
    let dir = scratch("force");
    let bundled = vec![dir.join("bundled")];
    let fetch = ConditionalFetch::new(big_po("Hello", "Bonjour"), "W/\"abc\"");
    let langs = vec!["fr".to_string()];

    // Install once so both a file and a validator exist.
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_000, false);
    assert_eq!(result.updated, langs);

    // Unforced: conditional, nothing changes.
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_100, false);
    assert!(result.updated.is_empty());

    // Forced (Python's Settings "Check for Updates"): the
    // validator is bypassed and the catalog is re-fetched.
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_200, true);
    assert_eq!(result.updated, langs);
    assert_eq!(fetch.seen(), ["", "W/\"abc\"", ""]);
}

#[test]
fn bundled_fallback_and_override_precedence() {
    let dir = scratch("fallback");
    let bundled_root = dir.join("bundled");
    // A bundled catalog on disk, as a release ships.
    let bundled_catalog = bundled_root.join("fr").join("LC_MESSAGES");
    std::fs::create_dir_all(&bundled_catalog).unwrap();
    std::fs::write(
        bundled_catalog.join("blindrss.po"),
        big_po("Hello", "Bundled"),
    )
    .unwrap();
    let bundled = vec![bundled_root.clone()];

    // No overrides at all: the bundled catalog loads and
    // translates (the fallback every failure path lands on).
    let cat = Catalog::load(&bundled_root, "fr");
    assert_eq!(cat.get("Hello 0"), "Bundled 0");

    // Same-version override precedence: a downloaded catalog
    // shadows the bundled one for the same msgid.
    let fetch = ConditionalFetch::new(big_po("Hello", "Downloaded"), "W/\"abc\"");
    let langs = vec!["fr".to_string()];
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_000, false);
    assert_eq!(result.updated, langs);
    let over = Catalog::load(&tu::override_root(&dir), "fr");
    assert_eq!(over.get("Hello 0"), "Downloaded 0");

    // A version transition removes the override; the bundled
    // catalog is what remains — intact, not blanked.
    assert_eq!(
        tu::prepare_overrides_for_app_version(&dir, &bundled, "9.9.9"),
        tu::PrepareOutcome::Reset
    );
    assert!(Catalog::load(&tu::override_root(&dir), "fr").is_empty());
    let cat = Catalog::load(&bundled_root, "fr");
    assert_eq!(cat.get("Hello 0"), "Bundled 0");

    // And when the reset cleanup fails, the bundled tree is
    // untouched by the failure (the app stands the override
    // down and reads bundled only; core records nothing).
    seed_state(&dir, Some("9.9.9"), None);
    std::fs::remove_dir_all(tu::override_root(&dir)).ok();
    std::fs::write(tu::override_root(&dir), b"not a directory").unwrap();
    assert_eq!(
        tu::prepare_overrides_for_app_version(&dir, &bundled, "10.0.0"),
        tu::PrepareOutcome::CleanupFailed
    );
    let cat = Catalog::load(&bundled_root, "fr");
    assert_eq!(cat.get("Hello 0"), "Bundled 0");
}
