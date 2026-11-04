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

/// Why a token request failed, split the way Python's
/// Inoreader provider splits it: a grant the server has
/// revoked (`invalid_grant`) is a different state from every
/// other failure — revoked credentials are cleared so the UI
/// shows "not authorized", transient failures keep them.
#[derive(Debug)]
enum TokenFailure {
    InvalidGrant,
    Other(String),
}

fn post_token_at(
    token_url: &str,
    form: &[(&str, String)],
) -> Result<serde_json::Value, TokenFailure> {
    let body: String = form
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut resp = crate::http::status_agent()
        .post(token_url)
        .header("Accept", "application/json")
        .send(&body)
        .map_err(|e| TokenFailure::Other(format!("token request: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| TokenFailure::Other(format!("token response: {e}")))?;
    if (200..300).contains(&status) {
        return serde_json::from_str(&text)
            .map_err(|e| TokenFailure::Other(format!("token response: {e}")));
    }
    // Python checks the error body of the HTTP error:
    // {"error": "invalid_grant"} means the refresh token was
    // revoked or expired for good.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
        && v.get("error").and_then(|e| e.as_str()) == Some("invalid_grant")
    {
        return Err(TokenFailure::InvalidGrant);
    }
    Err(TokenFailure::Other(format!(
        "token request returned HTTP {status}"
    )))
}

fn post_token(form: &[(&str, String)]) -> Result<serde_json::Value, String> {
    post_token_at(TOKEN_URL, form).map_err(|e| match e {
        TokenFailure::InvalidGrant => "token request: invalid_grant".to_string(),
        TokenFailure::Other(m) => m,
    })
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

/// Refresh against an explicit token endpoint (the production
/// caller passes TOKEN_URL; tests pass a loopback URL).
fn refresh_token_at(
    token_url: &str,
    app_id: &str,
    app_key: &str,
    refresh_token: &str,
) -> Result<serde_json::Value, TokenFailure> {
    post_token_at(
        token_url,
        &[
            ("client_id", app_id.to_string()),
            ("client_secret", app_key.to_string()),
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.to_string()),
        ],
    )
}

/// The Inoreader redirect URI the OAuth flows must use: the
/// configured `providers.inoreader.redirect_uri` when set,
/// else the loopback default (Python `gui/dialogs.py` supplies
/// the configured URI to both the authorization URL and the
/// code exchange — Audit 8 S15).
pub fn redirect_uri_from_config(cfg: &crate::config::Config) -> String {
    cfg.raw()
        .get("providers")
        .and_then(|p| p.get("inoreader"))
        .and_then(|s| s.get("redirect_uri"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_REDIRECT)
        .to_string()
}

/// The stored expiry for a token issued with `expires_in`
/// seconds of life (Python `_set_tokens`: now + expires_in
/// minus a 60-second safety margin; 0 = no known expiry when
/// the server gave none).
pub fn token_expires_at_from(expires_in: i64, now_unix: i64) -> i64 {
    if expires_in > 0 {
        now_unix + (expires_in - 60).max(0)
    } else {
        0
    }
}

/// Python `_token_is_stale`: no token is stale; with no
/// recorded expiry the token is taken at face value; otherwise
/// it is stale 60 seconds before the stored expiry.
pub fn token_is_stale(access_token: &str, token_expires_at: i64, now_unix: i64) -> bool {
    if access_token.is_empty() {
        return true;
    }
    if token_expires_at == 0 {
        return false;
    }
    now_unix >= token_expires_at - 60
}

/// What `InoreaderAuth::ensure_fresh` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAction {
    /// The stored access token is usable as-is (including
    /// Python's fall-back: a stale token with no refresh token
    /// is still presented — the server has the last word).
    Fresh,
    /// The access token was refreshed; any rotated refresh
    /// token and the new expiry are stored on the struct.
    Refreshed,
    /// The server revoked the grant (`invalid_grant`): the
    /// stored tokens were cleared, the account needs a fresh
    /// authorization in Settings.
    Revoked,
}

/// Inoreader's OAuth credential set and its refresh lifecycle
/// (Python `providers/inoreader.py` `_set_tokens`,
/// `_token_is_stale`, `_has_required_auth`). Before Audit 8
/// D07 the refresh helper had no production caller and the
/// provider presented the stored access token forever, so an
/// expired token silently dead-ended the account. Token values
/// are never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InoreaderAuth {
    pub app_id: String,
    pub app_key: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds; 0 = no recorded expiry.
    pub token_expires_at: i64,
    /// Overridable for tests.
    pub token_url: String,
}

impl InoreaderAuth {
    /// Load the credential set from `providers.inoreader`.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        let section = cfg.raw().get("providers").and_then(|p| p.get("inoreader"));
        let text = |key: &str| {
            section
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        // Python _parse_timestamp accepts numbers and numeric
        // strings; anything else is 0 (no recorded expiry).
        let expires = section
            .and_then(|s| s.get("token_expires_at"))
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_f64().map(|f| f as i64))
                    .or_else(|| {
                        v.as_str()
                            .and_then(|s| s.trim().parse::<f64>().ok())
                            .map(|f| f as i64)
                    })
            })
            .unwrap_or(0);
        Self {
            app_id: text("app_id"),
            app_key: text("app_key"),
            access_token: text("token"),
            refresh_token: text("refresh_token"),
            token_expires_at: expires,
            token_url: TOKEN_URL.to_string(),
        }
    }

    /// Write the credential set back to `providers.inoreader`
    /// (Python `_update_provider_config`).
    pub fn apply_to_config(&self, cfg: &mut crate::config::Config) {
        cfg.set_path(
            "providers.inoreader.token",
            serde_json::json!(self.access_token),
        );
        cfg.set_path(
            "providers.inoreader.refresh_token",
            serde_json::json!(self.refresh_token),
        );
        cfg.set_path(
            "providers.inoreader.token_expires_at",
            serde_json::json!(self.token_expires_at),
        );
    }

    /// Python `_has_app_credentials`.
    pub fn has_app_credentials(&self) -> bool {
        !self.app_id.is_empty() && !self.app_key.is_empty()
    }

    pub fn token_is_stale_at(&self, now_unix: i64) -> bool {
        token_is_stale(&self.access_token, self.token_expires_at, now_unix)
    }

    /// Make the access token usable, refreshing it when stale
    /// (Python `_has_required_auth`'s refresh branch). On
    /// `Refreshed`/`Revoked` the caller persists the struct
    /// (`apply_to_config` + save). A transient failure returns
    /// Err with the stored tokens untouched, so the working
    /// account cache is never discarded over a network blip.
    pub fn ensure_fresh(&mut self, now_unix: i64) -> Result<TokenAction, String> {
        if !self.token_is_stale_at(now_unix) {
            return Ok(TokenAction::Fresh);
        }
        if self.refresh_token.is_empty() {
            if self.access_token.is_empty() {
                return Err("Inoreader is not connected".to_string());
            }
            // Python: return bool(self.token) — present the
            // stale token; there is nothing to refresh with.
            return Ok(TokenAction::Fresh);
        }
        match refresh_token_at(
            &self.token_url,
            &self.app_id,
            &self.app_key,
            &self.refresh_token,
        ) {
            Ok(data) => {
                let access = data
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if access.is_empty() {
                    // Python falls through to bool(self.token):
                    // a response without a token changes nothing.
                    return Ok(TokenAction::Fresh);
                }
                let expires_in = data.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);
                self.access_token = access.to_string();
                if let Some(rotated) = data.get("refresh_token").and_then(|v| v.as_str()) {
                    self.refresh_token = rotated.to_string();
                }
                self.token_expires_at = token_expires_at_from(expires_in, now_unix);
                Ok(TokenAction::Refreshed)
            }
            Err(TokenFailure::InvalidGrant) => {
                // Revoked/expired grant: clear the tokens so
                // the account reads as not-authorized instead
                // of looping on every refresh (Python's
                // invalid_grant branch).
                self.access_token.clear();
                self.refresh_token.clear();
                self.token_expires_at = 0;
                Ok(TokenAction::Revoked)
            }
            Err(TokenFailure::Other(e)) => Err(e),
        }
    }

    /// `ensure_fresh` at the current time.
    pub fn ensure_fresh_now(&mut self) -> Result<TokenAction, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.ensure_fresh(now)
    }
}
