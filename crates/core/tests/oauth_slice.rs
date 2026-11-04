//! OAuth URL + redirect parse tests (no network).

use blindrss_core::config::Config;
use blindrss_core::oauth::{
    DEFAULT_REDIRECT, authorization_url, parse_oauth_redirect, redirect_uri_from_config,
    token_expires_at_from, token_is_stale,
};

#[test]
fn auth_url_shape() {
    let url = authorization_url("APPID", "https://127.0.0.1:18423/inoreader/oauth", "STATE1");
    assert!(url.starts_with("https://www.inoreader.com/oauth2/auth?"));
    assert!(url.contains("client_id=APPID"));
    assert!(url.contains("redirect_uri=https%3A%2F%2F127.0.0.1%3A18423%2Finoreader%2Foauth"));
    assert!(url.contains("state=STATE1"));
    assert!(url.contains("scope=read%20write"));
}

#[test]
fn redirect_parse() {
    let (code, state) =
        parse_oauth_redirect("https://127.0.0.1:18423/inoreader/oauth?code=abc%2F123&state=STATE1");
    assert_eq!(code.as_deref(), Some("abc/123"));
    assert_eq!(state.as_deref(), Some("STATE1"));
    let (code, _) = parse_oauth_redirect("code=only");
    assert_eq!(code.as_deref(), Some("only"));
}

#[test]
fn configured_redirect_uri_wins_over_default() {
    // Audit 8 S15: the configured URI feeds both OAuth flows.
    let mut cfg = Config::default();
    assert_eq!(redirect_uri_from_config(&cfg), DEFAULT_REDIRECT);
    cfg.set_path(
        "providers.inoreader.redirect_uri",
        serde_json::json!("  https://example.com/callback  "),
    );
    assert_eq!(
        redirect_uri_from_config(&cfg),
        "https://example.com/callback"
    );
    cfg.set_path("providers.inoreader.redirect_uri", serde_json::json!("   "));
    assert_eq!(redirect_uri_from_config(&cfg), DEFAULT_REDIRECT);
}

#[test]
fn token_staleness_matches_python() {
    // Python _token_is_stale: no token is stale; no recorded
    // expiry is taken at face value; otherwise stale 60
    // seconds before the stored expiry.
    assert!(token_is_stale("", 9_999_999, 100));
    assert!(!token_is_stale("tok", 0, 9_999_999));
    assert!(!token_is_stale("tok", 10_000, 9_939));
    assert!(token_is_stale("tok", 10_000, 9_940));
    assert!(token_is_stale("tok", 10_000, 20_000));
}

#[test]
fn token_expiry_storage_matches_python() {
    // Python _set_tokens: now + expires_in - 60; no lifetime
    // given means no recorded expiry.
    assert_eq!(token_expires_at_from(3600, 1_000), 1_000 + 3_540);
    assert_eq!(token_expires_at_from(0, 1_000), 0);
    assert_eq!(token_expires_at_from(-5, 1_000), 0);
    assert_eq!(token_expires_at_from(30, 1_000), 1_000);
}
