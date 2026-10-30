//! Help guide loading (docs/help).
//!
//! Python ships one Markdown guide per locale
//! (`docs/help/<locale>.md`, 17 files; the 92 topics are the
//! guide's `## ` sections). This module loads the guide for a
//! locale (falling back to English) and splits it into topics.

pub struct HelpTopic {
    pub title: String,
    pub body: String,
}

/// Load the guide text for `locale` from a help directory,
/// falling back to English, then to any available file.
pub fn load_guide(help_dir: &std::path::Path, locale: &str) -> Option<String> {
    for candidate in [locale, "en"] {
        let path = help_dir.join(format!("{candidate}.md"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(text);
        }
    }
    None
}

/// Split guide Markdown into topics on level-2 headings. Content
/// before the first `## ` becomes an "Introduction" topic.
pub fn topics(guide: &str) -> Vec<HelpTopic> {
    let mut out: Vec<HelpTopic> = Vec::new();
    let mut title = "Introduction".to_string();
    let mut body = String::new();
    for line in guide.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            let body_trim = body.trim().to_string();
            if !body_trim.is_empty() || !out.is_empty() {
                out.push(HelpTopic {
                    title: title.clone(),
                    body: body_trim,
                });
            }
            title = rest.trim().to_string();
            body = String::new();
        } else if line.starts_with("# ") && out.is_empty() && body.trim().is_empty() {
            // Skip the document title line.
            continue;
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    let body_trim = body.trim().to_string();
    if !body_trim.is_empty() || out.is_empty() {
        out.push(HelpTopic {
            title,
            body: body_trim,
        });
    }
    out
}

/// Light Markdown-to-plain-text cleanup for the reader view:
/// drop image syntax, unwrap links to their text, strip emphasis
/// markers and heading hashes.
pub fn to_plain_text(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        let mut text = line.trim_start_matches('#').trim_start().to_string();
        // [text](url) -> text
        while let Some(start) = text.find('[') {
            let Some(mid_rel) = text[start..].find("](") else {
                break;
            };
            let mid = start + mid_rel;
            let Some(end_rel) = text[mid..].find(')') else {
                break;
            };
            let end = mid + end_rel;
            let label = text[start + 1..mid].to_string();
            text = format!("{}{}{}", &text[..start], label, &text[end + 1..]);
        }
        let text = text.replace("**", "").replace('`', "");
        out.push_str(&text);
        out.push('\n');
    }
    out.trim().to_string()
}
