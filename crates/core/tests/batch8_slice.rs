//! Batch 8: provider depth — offline-verifiable behaviour.

use blindrss_core::config::Config;
use blindrss_core::provider::get_provider;
use blindrss_core::providers::{GReaderClient, GReaderService};

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn greader_client_builders() {
    let c = GReaderClient::new(GReaderService::BazQux, "a@b.c", "pw").with_cache_ttls(10, 99999);
    assert_eq!(c.service, GReaderService::BazQux);
    // Invalidation on a non-Inoreader service is a no-op.
    c.invalidate_caches();
    let ino = GReaderClient::with_oauth(GReaderService::Inoreader, "tok").with_cache_ttls(3600, 90);
    ino.invalidate_caches();
}

#[test]
fn youtube_sync_requires_signin() {
    let path = scratch("blindrss_batch8_yt.db");
    let db = blindrss_core::db::Db::open_or_create(&path).unwrap();
    let mut cfg = Config::default();
    let cfg_path = std::env::temp_dir().join("blindrss_batch8_cfg.json");
    let err = blindrss_core::youtube_account::sync_account(&mut cfg, &cfg_path, &db).unwrap_err();
    assert!(err.contains("not signed in"), "{err}");
    assert!(!blindrss_core::youtube_account::CLIENT_ID.is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn hosted_provider_miniflux_test_connection_needs_credentials() {
    let path = scratch("blindrss_batch8_mf.db");
    let _ = blindrss_core::db::Db::open_or_create(&path).unwrap();
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    let provider = get_provider(&cfg, path.clone());
    // No URL/key configured: refresh must fail with the
    // credentials message, not a panic.
    let err = provider.refresh().unwrap_err();
    assert!(err.contains("Miniflux"), "{err}");
    let _ = std::fs::remove_file(&path);
}
