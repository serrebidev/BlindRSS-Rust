//! OAuth URL + redirect parse tests (no network).

use blindrss_core::oauth::{authorization_url, parse_oauth_redirect};

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
