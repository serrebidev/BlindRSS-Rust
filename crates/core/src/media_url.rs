//! URL handling for Open Media URL — port of Python
//! core/media_url.py. One definition of "is this a usable URL":
//! clean a pasted string into an http(s) URL or reject it.

/// Characters people paste around a URL.
const WRAPPERS: &[char] = &[
    ' ', '\t', '\r', '\n', '<', '>', '"', '\'', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}',
];

const MEDIA_EXTENSIONS: &[&str] = &[
    ".mp3", ".m4a", ".m4b", ".aac", ".ogg", ".oga", ".opus", ".wav", ".flac", ".wma", ".aiff",
    ".aif", ".mp4", ".m4v", ".mkv", ".webm", ".mov", ".avi", ".wmv", ".flv", ".ts", ".m3u8",
    ".mpd",
];

fn has_other_scheme(raw: &str) -> bool {
    let Some((head, rest)) = raw.split_once(':') else {
        return false;
    };
    if head.contains('.') || head.contains('/') {
        return false;
    }
    let mut chars = head.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
        return false;
    }
    !rest.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// (scheme, authority, path) split of an absolute URL; path excludes
/// query and fragment.
fn split_url(url: &str) -> Option<(String, String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let after = &rest[authority_end..];
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    Some((
        scheme.to_string(),
        authority.to_string(),
        after[..path_end].to_string(),
    ))
}

/// Hostname from an authority: userinfo and port stripped,
/// lowercased (Python's urlsplit hostname).
fn hostname_of(authority: &str) -> String {
    let hostport = authority.rsplit('@').next().unwrap_or("");
    if let Some(rest) = hostport.strip_prefix('[') {
        // IPv6 literal.
        return rest.split(']').next().unwrap_or("").to_lowercase();
    }
    hostport.split(':').next().unwrap_or("").to_lowercase()
}

/// Clean a pasted string into an http(s) URL, or "" when it is not
/// one (Python's normalize_media_url).
pub fn normalize_media_url(text: &str) -> String {
    let mut raw = text
        .trim_matches(|c| WRAPPERS.contains(&c))
        .trim()
        .to_string();
    if raw.is_empty() || raw.chars().any(|c| c.is_whitespace()) {
        return String::new();
    }
    if let Some(stripped) = raw.strip_prefix("//") {
        raw = format!("https://{stripped}");
    } else if !raw.contains("://") {
        if has_other_scheme(&raw) {
            return String::new();
        }
        // Only guess a scheme when the leading segment looks like a
        // hostname; "how to bake bread" must not become a URL.
        let head = raw
            .split('/')
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("");
        let host = head.split(':').next().unwrap_or("");
        let host_ok = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'));
        if !host_ok || !host.contains('.') {
            return String::new();
        }
        raw = format!("https://{raw}");
    }
    let Some((scheme, authority, _)) = split_url(&raw) else {
        return String::new();
    };
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let host = hostname_of(&authority);
    if host.is_empty() || (!host.contains('.') && host != "localhost") {
        return String::new();
    }
    raw
}

pub fn is_media_url(text: &str) -> bool {
    !normalize_media_url(text).is_empty()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when the URL path ends in a media extension (Python's
/// looks_like_direct_media_file).
pub fn looks_like_direct_media_file(url: &str) -> bool {
    let normalized = normalize_media_url(url);
    if normalized.is_empty() {
        return false;
    }
    let Some((_, _, path)) = split_url(&normalized) else {
        return false;
    };
    let path = percent_decode(&path).to_lowercase();
    MEDIA_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

/// Best-effort human title from a URL; never empty (Python's
/// title_from_url).
pub fn title_from_url(url: &str) -> String {
    let normalized = normalize_media_url(url);
    if normalized.is_empty() {
        return "media".to_string();
    }
    let Some((_, authority, path)) = split_url(&normalized) else {
        return "media".to_string();
    };
    let decoded = percent_decode(&path);
    let name = decoded
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
    .trim();
    if !stem.is_empty() {
        // Runs of _ or + become one space.
        let mut out = String::new();
        let mut last_sep = false;
        for c in stem.chars() {
            if c == '_' || c == '+' {
                if !last_sep {
                    out.push(' ');
                }
                last_sep = true;
            } else {
                out.push(c);
                last_sep = false;
            }
        }
        let out = out.trim().to_string();
        if !out.is_empty() {
            return out;
        }
    }
    let host = hostname_of(&authority);
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.is_empty() {
        "media".to_string()
    } else {
        host.to_string()
    }
}
