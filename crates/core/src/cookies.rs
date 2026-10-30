//! Site cookies (port of the jar half of `core/site_cookies.py`).
//!
//! Python keeps a Netscape-format cookie jar (`site_cookies.txt`)
//! beside the config, harvested from the user's browsers or edited
//! by hand, and sends matching cookies on article fetches so
//! paywalled/clearance sites return real pages. This port covers
//! the jar format + per-URL matching + the header budget rule
//! (Python issue #101: keep the Cookie header under the smallest
//! common server limit, 8 KB).

use std::time::{SystemTime, UNIX_EPOCH};

pub const HEADER_BUDGET: usize = 8000;

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: i64, // unix seconds; 0 = session
    pub name: String,
    pub value: String,
}

/// Parse Netscape cookie-jar text. `#HttpOnly_` lines count as
/// cookies; plain `#` lines are comments.
pub fn parse_netscape_jar(text: &str) -> Vec<Cookie> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let line = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
            rest
        } else if line.starts_with('#') {
            continue;
        } else {
            line
        };
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        out.push(Cookie {
            domain: fields[0].trim().trim_start_matches('.').to_lowercase(),
            include_subdomains: fields[1].eq_ignore_ascii_case("TRUE"),
            path: fields[2].to_string(),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires: fields[4].trim().parse().unwrap_or(0),
            name: fields[5].to_string(),
            value: fields[6..].join("\t"),
        });
    }
    out
}

fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase()
}

fn path_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    match rest.find('/') {
        Some(i) => rest[i..]
            .split(['?', '#'])
            .next()
            .unwrap_or("/")
            .to_string(),
        None => "/".to_string(),
    }
}

fn cookie_matches(cookie: &Cookie, host: &str, path: &str, https: bool, now: i64) -> bool {
    if cookie.expires != 0 && cookie.expires < now {
        return false;
    }
    if cookie.secure && !https {
        return false;
    }
    let domain_ok = host == cookie.domain
        || (cookie.include_subdomains && host.ends_with(&format!(".{}", cookie.domain)));
    if !domain_ok {
        return false;
    }
    path.starts_with(&cookie.path)
}

/// The `Cookie` header value for `url` from the jar, trimmed to
/// the header budget (later cookies dropped first, like Python).
pub fn cookie_header_for(jar: &[Cookie], url: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let host = host_of(url);
    let path = path_of(url);
    let https = url.starts_with("https://");
    let mut header = String::new();
    for cookie in jar {
        if !cookie_matches(cookie, &host, &path, https, now) {
            continue;
        }
        let piece = format!("{}={}", cookie.name, cookie.value);
        let extra = if header.is_empty() {
            piece.len()
        } else {
            piece.len() + 2
        };
        if header.len() + extra > HEADER_BUDGET {
            break;
        }
        if !header.is_empty() {
            header.push_str("; ");
        }
        header.push_str(&piece);
    }
    header
}

/// Load the jar that sits beside the config file
/// (`site_cookies.txt`); empty when absent.
pub fn load_jar_beside(config_path: &std::path::Path) -> Vec<Cookie> {
    let Some(dir) = config_path.parent() else {
        return Vec::new();
    };
    match std::fs::read_to_string(dir.join("site_cookies.txt")) {
        Ok(text) => parse_netscape_jar(&text),
        Err(_) => Vec::new(),
    }
}
