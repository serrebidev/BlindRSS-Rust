//! Content-language resolution — port of Python core/article_lang.py
//! (issue #72). A wrong language is worse than none: it points
//! assistive tech at the wrong synthesizer, so every source is
//! validated as a BCP-47 tag before use.

pub const DEFAULT_LANG: &str = "en";

fn bcp47_shape_ok(text: &str) -> bool {
    // ^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})*$
    let mut parts = text.split('-');
    let first = parts.next().unwrap_or("");
    if !(2..=3).contains(&first.len()) || !first.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts.all(|p| (2..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Return a clean BCP-47 tag, or None when the value is not usable
/// (Python's normalize_lang).
pub fn normalize_lang(value: Option<&str>) -> Option<String> {
    let value = value?;
    let mut text = value.trim().to_string();
    if text.is_empty() {
        return None;
    }
    // Drop a POSIX charset/modifier suffix.
    if let Some(i) = text.find(['.', '@']) {
        text.truncate(i);
    }
    let text = text.replace('_', "-").trim_matches('-').to_string();
    if text.is_empty() || !bcp47_shape_ok(&text) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for (i, part) in text.split('-').enumerate() {
        if i == 0 {
            out.push(part.to_lowercase());
        } else if part.len() == 2 {
            out.push(part.to_uppercase());
        } else if part.len() == 4 {
            let mut c = part.chars();
            let first = c.next().unwrap_or(' ').to_uppercase().collect::<String>();
            out.push(format!("{first}{}", c.as_str().to_lowercase()));
        } else {
            out.push(part.to_lowercase());
        }
    }
    Some(out.join("-"))
}

/// Language declared by a source page's <html> tag, or None
/// (Python's lang_from_page_html).
pub fn lang_from_page_html(page_html: &str) -> Option<String> {
    if page_html.is_empty() {
        return None;
    }
    let lower = page_html.to_lowercase();
    // Find "<html" at a word boundary.
    let mut search = 0;
    let tag_start = loop {
        let idx = lower[search..].find("<html").map(|i| i + search)?;
        let after = lower.as_bytes().get(idx + 5).copied();
        match after {
            Some(b) if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' => {
                search = idx + 5;
                continue;
            }
            _ => break idx,
        }
    };
    let tag_end = page_html[tag_start..]
        .find('>')
        .map(|i| i + tag_start)
        .unwrap_or(page_html.len());
    let tag = &page_html[tag_start..tag_end];
    let tag_lower = tag.to_lowercase();
    // Find lang= / xml:lang= at a word boundary.
    let mut search = 0;
    while let Some(rel) = tag_lower[search..].find("lang") {
        let idx = search + rel;
        let before_ok =
            idx == 0 || !matches!(tag_lower.as_bytes()[idx - 1], b'a'..=b'z' | b'0'..=b'9' | b'_');
        search = idx + 4;
        if !before_ok {
            continue;
        }
        let rest = tag[idx + 4..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix('"')
            .or_else(|| rest.strip_prefix('\''))
            .unwrap_or(rest);
        // Capture [A-Za-z]{1,8}(-[A-Za-z0-9]{1,8})*
        let mut value = String::new();
        let mut chars = rest.chars().peekable();
        let mut first = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_alphabetic() && first.len() < 8 {
                first.push(*c);
                chars.next();
            } else {
                break;
            }
        }
        if first.is_empty() {
            continue;
        }
        value.push_str(&first);
        while chars.peek() == Some(&'-') {
            let mut segment = String::new();
            let mut peek_iter = chars.clone();
            peek_iter.next();
            while let Some(c) = peek_iter.peek() {
                if c.is_ascii_alphanumeric() && segment.len() < 8 {
                    segment.push(*c);
                    peek_iter.next();
                } else {
                    break;
                }
            }
            if segment.is_empty() {
                break;
            }
            chars = peek_iter;
            value.push('-');
            value.push_str(&segment);
        }
        return normalize_lang(Some(&value));
    }
    None
}

/// The candidate sources for resolve_content_language, in Python's
/// priority order.
#[derive(Debug, Default, Clone)]
pub struct ContentLangSources {
    /// 1. What the text was translated INTO.
    pub translation_target: Option<String>,
    /// 2. Explicit page language.
    pub page_lang: Option<String>,
    /// 2 (convenience). Raw source page HTML.
    pub page_html: Option<String>,
    /// 3a. Per-item xml:lang.
    pub feed_item_lang: Option<String>,
    /// 3b. Channel <language>.
    pub feed_lang: Option<String>,
    /// 4. Caller's fallback.
    pub fallback: Option<String>,
}

/// Pick the content language by issue #72's priority order; the
/// final fallback is the app UI language (passed in), then "en".
pub fn resolve_content_language(sources: &ContentLangSources, ui_language: &str) -> String {
    let from_html = sources.page_html.as_deref().and_then(lang_from_page_html);
    let candidates: [Option<&str>; 6] = [
        sources.translation_target.as_deref(),
        sources.page_lang.as_deref(),
        from_html.as_deref(),
        sources.feed_item_lang.as_deref(),
        sources.feed_lang.as_deref(),
        sources.fallback.as_deref(),
    ];
    for candidate in candidates {
        if let Some(tag) = normalize_lang(candidate) {
            return tag;
        }
    }
    normalize_lang(Some(ui_language)).unwrap_or_else(|| DEFAULT_LANG.to_string())
}
