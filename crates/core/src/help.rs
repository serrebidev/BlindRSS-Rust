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

/// The base language of a locale code (Python
/// `_base_language`): `pt-BR` / `pt_BR` -> `pt`, lowercased.
pub fn base_language(code: &str) -> String {
    let text = code.trim().replace('_', "-");
    match text.split('-').next() {
        Some(base) => base.to_lowercase(),
        None => String::new(),
    }
}

/// Locale codes to try for a guide, most specific first,
/// English last (Python `_language_candidates`, Audit 8 S08):
/// the code as given (with `_` normalized to `-`), the
/// underscore form, the lowercased form, the base language,
/// then English — deduplicated, order preserved.
pub fn language_candidates(language: &str) -> Vec<String> {
    let text = language.trim().replace('_', "-");
    let mut out: Vec<String> = Vec::new();
    let mut add = |value: String| {
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
    };
    add(text.clone());
    add(text.replace('-', "_"));
    add(text.to_lowercase());
    add(base_language(&text));
    add("en".to_string());
    out
}

/// One block inside a guide section (Python `Section.blocks`).
#[derive(Debug, Clone, PartialEq)]
pub enum HelpBlock {
    Text(String),
    Bullet(String),
    Number(String),
    Code(String),
}

/// One heading and the body blocks that follow it.
#[derive(Debug, Clone, PartialEq)]
pub struct HelpSection {
    pub anchor: String,
    pub title: String,
    pub level: usize,
    pub blocks: Vec<HelpBlock>,
}

/// A parsed guide (Python `HelpDocument`).
#[derive(Debug, Clone, PartialEq)]
pub struct HelpDocument {
    /// The language actually loaded (a candidate code).
    pub language: String,
    /// The language the caller asked for.
    pub requested_language: String,
    pub title: String,
    pub sections: Vec<HelpSection>,
}

impl HelpDocument {
    /// True when the requested language had no guide and a
    /// different language (in practice English) was used —
    /// the viewer announces this (Audit 8 S08).
    pub fn is_fallback(&self) -> bool {
        base_language(&self.requested_language) != base_language(&self.language)
    }

    pub fn section(&self, anchor: &str) -> Option<&HelpSection> {
        self.sections.iter().find(|s| s.anchor == anchor)
    }
}

/// Flatten inline Markdown to speech-friendly plain text
/// (Python `strip_inline_markup`): links become
/// `text (url)`, bold/emphasis/code markers are unwrapped.
pub fn strip_inline_markup(text: &str) -> String {
    // Links: [label](url) or [label](url "title") -> label (url)
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == ']')
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = (close + 2..chars.len()).find(|&j| chars[j] == ')')
        {
            let inner: String = chars[close + 2..end].iter().collect();
            let url = inner.split_whitespace().next().unwrap_or("");
            if !url.is_empty() {
                let label: String = chars[i + 1..close].iter().collect();
                out.push_str(&label);
                out.push_str(" (");
                out.push_str(url);
                out.push(')');
                i = end + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    // Bold (**x**) before emphasis (*x*), then code (`x`).
    let out = unwrap_pairs(&out, "**");
    let out = unwrap_pairs(&out, "`");
    let out = unwrap_pairs(&out, "*");
    out.trim().to_string()
}

/// Remove paired `marker` spans, keeping the inner text.
/// An unmatched marker is left as-is.
fn unwrap_pairs(text: &str, marker: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        match after.find(marker) {
            Some(end_rel) if end_rel > 0 => {
                out.push_str(&rest[..start]);
                out.push_str(&after[..end_rel]);
                rest = &after[end_rel + marker.len()..];
            }
            _ => break,
        }
    }
    out.push_str(rest);
    out
}

/// Split a heading line into (level, title, anchor) when it
/// matches Python's heading shape: 1-6 leading `#`, whitespace,
/// a title, and an optional trailing `{#anchor}`.
fn parse_heading(line: &str) -> Option<(usize, String, String)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut text = rest.trim();
    let mut anchor = String::new();
    if text.ends_with('}')
        && let Some(open) = text.rfind("{#")
    {
        let candidate = &text[open + 2..text.len() - 1];
        if !candidate.is_empty()
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            anchor = candidate.to_string();
            text = text[..open].trim();
        }
    }
    Some((hashes, strip_inline_markup(text), anchor))
}

/// Parse guide Markdown into a document (Python `parse`).
pub fn parse_guide(text: &str, language: &str) -> HelpDocument {
    let mut title = String::new();
    let mut sections: Vec<HelpSection> = Vec::new();
    let mut in_code_block = false;
    let mut paragraph: Vec<String> = Vec::new();

    fn flush(paragraph: &mut Vec<String>, sections: &mut [HelpSection]) {
        if !paragraph.is_empty() {
            if let Some(current) = sections.last_mut() {
                let joined = paragraph.join(" ");
                current
                    .blocks
                    .push(HelpBlock::Text(strip_inline_markup(&joined)));
            }
            paragraph.clear();
        }
    }

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim_start().starts_with("```") {
            flush(&mut paragraph, &mut sections);
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            if let Some(current) = sections.last_mut() {
                current.blocks.push(HelpBlock::Code(raw_line.to_string()));
            }
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut paragraph, &mut sections);
            continue;
        }
        if let Some((level, heading_title, anchor)) = parse_heading(line) {
            flush(&mut paragraph, &mut sections);
            if level == 1 && sections.is_empty() && anchor.is_empty() {
                // A bare "# Title" before the first section is
                // the document title, not a section.
                title = heading_title;
                continue;
            }
            if level == 1 && title.is_empty() {
                title = heading_title.clone();
            }
            sections.push(HelpSection {
                anchor,
                title: heading_title,
                level,
                blocks: Vec::new(),
            });
            continue;
        }
        let trimmed = line.trim_start();
        let bullet_body = ["-", "*", "+"].iter().find_map(|m| {
            trimmed
                .strip_prefix(m)
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .map(|rest| rest.trim_start())
        });
        if let Some(body) = bullet_body {
            flush(&mut paragraph, &mut sections);
            if let Some(current) = sections.last_mut() {
                current
                    .blocks
                    .push(HelpBlock::Bullet(strip_inline_markup(body)));
            }
            continue;
        }
        // Numbered list item: digits, '.' or ')', whitespace.
        let digit_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digit_len > 0 {
            let after = &trimmed[digit_len..];
            if let Some(rest) = after
                .strip_prefix(['.', ')'])
                .filter(|rest| rest.starts_with(char::is_whitespace))
            {
                flush(&mut paragraph, &mut sections);
                if let Some(current) = sections.last_mut() {
                    let num = &trimmed[..digit_len];
                    current.blocks.push(HelpBlock::Number(format!(
                        "{num}. {}",
                        strip_inline_markup(rest.trim_start())
                    )));
                }
                continue;
            }
        }
        paragraph.push(line.trim().to_string());
    }
    flush(&mut paragraph, &mut sections);
    HelpDocument {
        language: language.to_string(),
        requested_language: language.to_string(),
        title,
        sections,
    }
}

/// The flattened guide (Python `render_plain_text`): the full
/// text, a map of anchor -> zero-based line, and the start
/// line of every section, parallel to `contents_entries`.
pub struct RenderedGuide {
    pub text: String,
    pub anchor_lines: std::collections::HashMap<String, usize>,
    pub section_lines: Vec<usize>,
}

pub fn render_plain_text(document: &HelpDocument) -> RenderedGuide {
    let mut lines: Vec<String> = Vec::new();
    let mut anchor_lines: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut section_lines: Vec<usize> = Vec::new();

    let first_title = document
        .sections
        .first()
        .map(|s| s.title.as_str())
        .unwrap_or("");
    if !document.title.is_empty() && document.title != first_title {
        lines.push(document.title.clone());
        lines.push(String::new());
    }
    for section in &document.sections {
        if lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        section_lines.push(lines.len());
        if !section.anchor.is_empty() {
            anchor_lines.insert(section.anchor.clone(), lines.len());
        }
        lines.push(section.title.clone());
        lines.push(String::new());
        for block in &section.blocks {
            match block {
                HelpBlock::Bullet(text) => lines.push(format!("- {text}")),
                HelpBlock::Code(text) => lines.push(format!("    {text}")),
                HelpBlock::Text(text) | HelpBlock::Number(text) => {
                    lines.push(text.clone());
                    lines.push(String::new());
                }
            }
        }
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    RenderedGuide {
        text,
        anchor_lines,
        section_lines,
    }
}

/// `(anchor, display title, level)` for the contents list
/// (Python `contents_entries`).
pub fn contents_entries(document: &HelpDocument) -> Vec<(&str, &str, usize)> {
    document
        .sections
        .iter()
        .map(|s| (s.anchor.as_str(), s.title.as_str(), s.level))
        .collect()
}

/// Load and parse the guide for `locale` from a help
/// directory, walking the full fallback chain (Audit 8 S08).
/// Never fails: a missing guide yields an empty document in
/// the fallback language so the viewer can say so.
pub fn load_document(help_dir: &std::path::Path, locale: &str) -> HelpDocument {
    for candidate in language_candidates(locale) {
        let path = help_dir.join(format!("{candidate}.md"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            // Python reads utf-8-sig: drop a leading BOM.
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let mut document = parse_guide(text, &candidate);
            document.requested_language = locale.to_string();
            return document;
        }
    }
    HelpDocument {
        language: "en".to_string(),
        requested_language: locale.to_string(),
        title: String::new(),
        sections: Vec::new(),
    }
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
