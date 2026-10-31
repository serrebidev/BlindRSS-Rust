//! Help guide loading (docs/help).
//!
//! Python ships one Markdown guide per locale
//! (`docs/help/<locale>.md`, 17 files; the 92 topics are the
//! guide's `## ` sections). This module loads the guide for a
//! locale (falling back to English) and splits it into topics.

pub struct HelpTopic {
    /// The section anchor (`{#main-window}` -> "main-window");
    /// empty for the pre-guide Introduction topic.
    pub id: String,
    pub title: String,
    pub body: String,
}

/// Split a guide heading into (title, anchor id): headings are
/// `## Title {#topic-id}` in every shipped locale.
fn split_heading(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if let Some(open) = rest.rfind("{#")
        && let Some(close) = rest.rfind('}')
        && close > open
    {
        return (
            rest[..open].trim().to_string(),
            rest[open + 2..close].trim().to_string(),
        );
    }
    (rest.to_string(), String::new())
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
    let mut id = String::new();
    let mut body = String::new();
    for line in guide.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            let body_trim = body.trim().to_string();
            if !body_trim.is_empty() || !out.is_empty() {
                out.push(HelpTopic {
                    id: id.clone(),
                    title: title.clone(),
                    body: body_trim,
                });
            }
            let (new_title, new_id) = split_heading(rest);
            title = new_title;
            id = new_id;
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
            id,
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
