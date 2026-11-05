//! Gettext catalogs (i18n slice).
//!
//! Python ships `.po` files for 17 locales under `locale/`.
//! This module parses the PO format directly (msgid/msgstr,
//! multiline strings, C escapes, plural/context entries skipped
//! to their base form) into a lookup map. Translation falls back
//! to the msgid itself, matching gettext behaviour.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct Catalog {
    entries: HashMap<String, String>,
}

fn unescape_po(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn po_string(line: &str) -> Option<String> {
    let line = line.trim();
    let inner = line.strip_prefix('"')?.strip_suffix('"')?;
    Some(unescape_po(inner))
}

impl Catalog {
    /// Parse PO source text into a catalog.
    pub fn parse(po_text: &str) -> Catalog {
        let mut catalog = Catalog::default();
        let mut msgid: Option<String> = None;
        let mut msgstr = String::new();
        let mut state = 0; // 0 idle, 1 in msgid, 2 in msgstr
        let flush = |catalog: &mut Catalog, msgid: &Option<String>, msgstr: &str| {
            if let Some(id) = msgid
                && !id.is_empty()
                && !msgstr.is_empty()
            {
                catalog.entries.insert(id.clone(), msgstr.to_string());
            }
        };
        for raw in po_text.lines() {
            let line = raw.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("msgid ") {
                flush(&mut catalog, &msgid, &msgstr);
                msgid = po_string(rest);
                msgstr = String::new();
                state = 1;
            } else if let Some(rest) = line.strip_prefix("msgstr ") {
                if let Some(s) = po_string(rest) {
                    msgstr = s;
                }
                state = 2;
            } else if line.starts_with("msgstr[") {
                state = 2;
            } else if line.starts_with('"')
                && let Some(s) = po_string(line)
            {
                match state {
                    1 => {
                        if let Some(id) = &mut msgid {
                            id.push_str(&s);
                        }
                    }
                    2 => msgstr.push_str(&s),
                    _ => {}
                }
            }
        }
        flush(&mut catalog, &msgid, &msgstr);
        catalog
    }

    /// Load `locale/<lang>/LC_MESSAGES/blindrss.po` from a locale
    /// root directory; empty catalog when missing.
    pub fn load(locale_root: &std::path::Path, lang: &str) -> Catalog {
        let path = locale_root
            .join(lang)
            .join("LC_MESSAGES")
            .join("blindrss.po");
        match std::fs::read_to_string(&path) {
            Ok(text) => Catalog::parse(&text),
            Err(_) => Catalog::default(),
        }
    }

    /// Translate; falls back to the source string.
    pub fn get<'a>(&'a self, msgid: &'a str) -> &'a str {
        self.entries.get(msgid).map(|s| s.as_str()).unwrap_or(msgid)
    }

    /// Language codes with a catalog on disk under any of the
    /// given locale roots (Python `available_languages`, for
    /// the Settings interface-language picker, Audit 8 S09):
    /// a code counts when its `LC_MESSAGES/blindrss.po`
    /// exists, mirroring `Catalog::load`'s path. Sorted,
    /// deduplicated; English is NOT force-added here (the
    /// picker adds it, since the source strings always exist).
    pub fn available_languages(roots: &[std::path::PathBuf]) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            names.sort();
            for name in names {
                let catalog = root.join(&name).join("LC_MESSAGES").join("blindrss.po");
                if catalog.is_file() && !found.contains(&name) {
                    found.push(name);
                }
            }
        }
        found.sort();
        found
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
