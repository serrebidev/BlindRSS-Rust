//! Inoreader OAuth (port of `core/inoreader_oauth.py`).
//!
//! Authorization URL construction, redirect parsing (Python
//! supports pasting the redirect URL by hand, which is the path
//! this port uses — no local listener needed), code exchange, and
//! refresh. Tokens live in config under providers.inoreader.

pub const AUTH_URL: &str = "https://www.inoreader.com/oauth2/auth";
pub const TOKEN_URL: &str = "https://www.inoreader.com/oauth2/token";
pub const DEFAULT_REDIRECT: &str = "https://127.0.0.1:18423/inoreader/oauth";
pub const DEFAULT_SCOPE: &str = "read write";

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Build the authorization URL. State is derived from the app id
/// and a caller-supplied nonce (Python uses a random token; the
/// caller owns nonce generation so this stays pure/testable).
pub fn authorization_url(app_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&state={}&scope={}",
        url_encode(app_id),
        url_encode(redirect_uri),
        url_encode(state),
        url_encode(DEFAULT_SCOPE),
    )
}

/// Extract (code, state) from a pasted redirect URL or bare query.
pub fn parse_oauth_redirect(value: &str) -> (Option<String>, Option<String>) {
    let query = value.split('?').nth(1).unwrap_or(value);
    let query = query.split('#').next().unwrap_or(query);
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let decoded = percent_decode(v);
        match k {
            "code" => code = Some(decoded),
            "state" => state = Some(decoded),
            _ => {}
        }
    }
    (code, state)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn post_token(form: &[(&str, String)]) -> Result<serde_json::Value, String> {
    let body: String = form
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut resp = ureq::post(TOKEN_URL)
        .header("Accept", "application/json")
        .send(&body)
        .map_err(|e| format!("token request: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("token response: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("token response: {e}"))
}

/// Exchange an authorization code for tokens.
pub fn exchange_code(
    app_id: &str,
    app_key: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<serde_json::Value, String> {
    post_token(&[
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", app_id.to_string()),
        ("client_secret", app_key.to_string()),
        ("grant_type", "authorization_code".to_string()),
    ])
}

/// Refresh an access token.
pub fn refresh_token(
    app_id: &str,
    app_key: &str,
    refresh_token: &str,
) -> Result<serde_json::Value, String> {
    post_token(&[
        ("client_id", app_id.to_string()),
        ("client_secret", app_key.to_string()),
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
    ])
}
