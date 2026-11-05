//! OPML import/export.
//!
//! Import mirrors Python's two implementations (Audit 8 Task
//! 27, F13): `providers/local.py::import_opml` for the local
//! database (category rows ensured, OPML titles preserved as
//! custom, feed URLs resolved offline, duplicates detected on
//! the original AND resolved URL) and `core/utils.py::
//! parse_opml` for the (title, url, category) triples the
//! hosted providers' base import consumes. Both share Python's
//! front end: the encoding fallback chain and a tolerant,
//! recoverable parse with case-insensitive attribute lookup —
//! the strict `opml` crate parse the old port used rejected
//! real-world files (uppercase attributes, unclosed outlines,
//! cp1252 bytes) that Python accepts.
//!
//! Export keeps the `opml` crate writer: category paths nest
//! as outline hierarchies, the shape import reads back.

use crate::db::{CATEGORY_PATH_SEP, Db, Feed, make_category_path, sanitize_category_leaf};
use opml::{OPML, Outline};

/// The system category sentinel, stored literally (Python
/// `core.db.UNCATEGORIZED`).
pub const UNCATEGORIZED: &str = "Uncategorized";

/// Python's OPML read chain: utf-8, then utf-8-sig, then
/// latin-1, then cp1252 — the first decode that does not fail
/// wins. latin-1 cannot fail, so in practice the chain is
/// "strict utf-8, else latin-1" (cp1252 is unreachable, exactly
/// as in Python; a cp1252 file's smart quotes therefore land
/// as the latin-1 control characters Python also stores).
/// A utf-8 BOM survives the first step as U+FEFF and is
/// stripped here, which is the utf-8-sig step's net effect.
pub fn decode_opml(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text).to_string(),
        Err(_) => bytes.iter().map(|b| *b as char).collect(),
    }
}

/// One outline element: its attributes (original key case
/// preserved; lookup is case-insensitive, like Python's
/// helpers) and its child outlines.
#[derive(Debug, Clone, Default)]
struct OutlineNode {
    attrs: Vec<(String, String)>,
    children: Vec<OutlineNode>,
}

impl OutlineNode {
    /// Python's `get_attr`: exact key first, then a
    /// case-insensitive scan.
    fn attr(&self, name: &str) -> Option<&str> {
        if let Some((_, v)) = self.attrs.iter().find(|(k, _)| k == name) {
            return Some(v);
        }
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn imported_title(&self) -> String {
        self.attr("text")
            .or_else(|| self.attr("title"))
            .unwrap_or("")
            .to_string()
    }

    fn xml_url(&self) -> String {
        self.attr("xmlUrl").unwrap_or("").to_string()
    }
}

/// Parse-time outline: children are arena indexes into the
/// node vector (an outline's children can be completed before
/// the outline itself is, when end tags are missing).
struct ArenaNode {
    attrs: Vec<(String, String)>,
    children: Vec<usize>,
}

enum Open {
    Body,
    Outline(usize),
    Other(String),
}

/// Tolerantly tokenize the document and return the outlines
/// under `<body>` (BeautifulSoup's parsers recover unclosed or
/// mis-nested elements; this tokenizer never requires balanced
/// tags — an end tag closes everything opened inside it, and
/// EOF closes the rest). Err when no `<body>` exists, which is
/// Python's failure condition in both implementations.
fn parse_outline_tree(text: &str) -> Result<Vec<OutlineNode>, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    let mut arena: Vec<ArenaNode> = Vec::new();
    let mut roots: Vec<usize> = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut body_seen = false;
    let mut buf = Vec::new();
    let read_attrs = |e: &quick_xml::events::BytesStart<'_>| -> Vec<(String, String)> {
        e.attributes()
            .filter_map(|a| a.ok())
            .map(|a| {
                (
                    a.key.as_ref().to_string(),
                    a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap_or_default()
                        .into_owned(),
                )
            })
            .collect()
    };
    let open_outline = |arena: &mut Vec<ArenaNode>,
                        roots: &mut Vec<usize>,
                        stack: &[Open],
                        attrs: Vec<(String, String)>|
     -> usize {
        let idx = arena.len();
        arena.push(ArenaNode {
            attrs,
            children: Vec::new(),
        });
        match stack.iter().rev().find_map(|o| match o {
            Open::Outline(parent) => Some(*parent),
            _ => None,
        }) {
            Some(parent) => arena[parent].children.push(idx),
            None => roots.push(idx),
        }
        idx
    };
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_lowercase();
                if name == "body" && !body_seen {
                    body_seen = true;
                    stack.push(Open::Body);
                } else if name == "outline" && stack.iter().any(|o| matches!(o, Open::Body)) {
                    let idx = open_outline(&mut arena, &mut roots, &stack, read_attrs(&e));
                    stack.push(Open::Outline(idx));
                } else {
                    stack.push(Open::Other(name));
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name().as_ref().to_lowercase();
                if name == "body" {
                    body_seen = true;
                } else if name == "outline" && stack.iter().any(|o| matches!(o, Open::Body)) {
                    open_outline(&mut arena, &mut roots, &stack, read_attrs(&e));
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_lowercase();
                let pos = stack.iter().rposition(|o| match o {
                    Open::Body => name == "body",
                    Open::Outline(_) => name == "outline",
                    Open::Other(n) => *n == name,
                });
                if let Some(pos) = pos {
                    stack.truncate(pos);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // recovery: keep whatever was read
            _ => {}
        }
        buf.clear();
    }
    if !body_seen {
        return Err("OPML body is missing".to_string());
    }
    fn assemble(idx: usize, arena: &[ArenaNode]) -> OutlineNode {
        let node = &arena[idx];
        OutlineNode {
            attrs: node.attrs.clone(),
            children: node
                .children
                .iter()
                .map(|&child| assemble(child, arena))
                .collect(),
        }
    }
    Ok(roots.iter().map(|&idx| assemble(idx, &arena)).collect())
}

/// Python `_opml_append_category` / the local import's
/// `append_category`: folder titles may themselves contain
/// path separators; each leaf is sanitized, empty leaves are
/// skipped, and the Uncategorized parent contributes nothing.
fn append_category(parent: &str, folder_title: &str) -> String {
    let mut path = if parent == UNCATEGORIZED {
        String::new()
    } else {
        parent.trim().to_string()
    };
    for part in folder_title.split(CATEGORY_PATH_SEP) {
        let leaf = sanitize_category_leaf(part);
        if !leaf.is_empty() {
            path = make_category_path(
                if path.is_empty() {
                    None
                } else {
                    Some(path.as_str())
                },
                &leaf,
            );
        }
    }
    if path.is_empty() {
        UNCATEGORIZED.to_string()
    } else {
        path
    }
}

/// Normalize a caller-supplied category path the way Python's
/// `ensure_category_path` does: split, sanitize leaves, rejoin.
fn sanitize_path(path: &str) -> String {
    let mut out = String::new();
    for part in path.split(CATEGORY_PATH_SEP) {
        let leaf = sanitize_category_leaf(part);
        if !leaf.is_empty() {
            out = make_category_path(
                if out.is_empty() {
                    None
                } else {
                    Some(out.as_str())
                },
                &leaf,
            );
        }
    }
    out
}

/// Python `providers/local.py::_resolve_feed_url` with
/// `allow_network=False`: normalize the submitted URL, replace
/// Google Groups forum forms with the synthetic subscription
/// URL, keep Groups.io searches / YouTube searches /
/// SoundCloud and Mixcloud listings verbatim, and normalize
/// Rumble/Odysee feed URLs — all offline.
pub fn resolve_feed_url_offline(url: &str) -> String {
    // utils.normalize_user_submitted_url: strip whitespace,
    // one wrapping <...>, and stray backticks.
    let mut resolved = url.trim().to_string();
    if resolved.len() >= 2 && resolved.starts_with('<') && resolved.ends_with('>') {
        resolved = resolved[1..resolved.len() - 1].trim().to_string();
    }
    resolved = resolved.trim_matches('`').trim().to_string();
    if resolved.is_empty() {
        return String::new();
    }
    if let Some(group) = crate::sources::google_group_subscription_url(&resolved) {
        return group;
    }
    if crate::sources::groups_io_search(&resolved).is_some() {
        return resolved;
    }
    if crate::sources::youtube_search_query(&resolved).is_some() {
        return resolved;
    }
    if matches!(
        crate::sources::soundcloud_listing_kind(&resolved),
        "user" | "playlist"
    ) {
        return resolved;
    }
    if let Some((kind, _)) = crate::sources::mixcloud_listing(&resolved)
        && matches!(kind, "user" | "playlist")
    {
        return resolved;
    }
    let resolved = crate::sources::normalize_rumble_feed_url(&resolved);
    let resolved = crate::sources::normalize_odysee_url(&resolved);
    let resolved = resolved.trim().to_string();
    if resolved.is_empty() {
        url.trim().to_string()
    } else {
        resolved
    }
}

/// Import subscriptions from OPML bytes into the local
/// database (Python `LocalProvider.import_opml`). Returns how
/// many feeds were added; Err when the document has no body.
pub fn import_opml(db: &Db, bytes: &[u8]) -> Result<usize, String> {
    import_opml_under(db, bytes, None)
}

/// Import OPML under a target category ("Import OPML Here"):
/// the target becomes the base category the document's own
/// folders extend (Python's semantics — the target itself is
/// ensured as a category path first; the Uncategorized
/// sentinel target means "no target").
pub fn import_opml_under(db: &Db, bytes: &[u8], target: Option<&str>) -> Result<usize, String> {
    let text = decode_opml(bytes);
    let roots = parse_outline_tree(&text)?;
    let base = match target {
        Some(target) if !target.trim().is_empty() && target.trim() != UNCATEGORIZED => {
            let path = sanitize_path(target);
            if path.is_empty() {
                UNCATEGORIZED.to_string()
            } else {
                db.ensure_category_path(&path).map_err(|e| format!("{e}"))?;
                path
            }
        }
        _ => UNCATEGORIZED.to_string(),
    };
    let mut added = 0;
    for node in &roots {
        import_outline(db, node, &base, &mut added)?;
    }
    Ok(added)
}

fn import_outline(
    db: &Db,
    node: &OutlineNode,
    current_category: &str,
    added: &mut usize,
) -> Result<(), String> {
    let imported_title = node.imported_title().trim().to_string();
    let text = if imported_title.is_empty() {
        "Unknown Feed".to_string()
    } else {
        imported_title.clone()
    };
    let xml_url = node.xml_url().trim().to_string();
    if !xml_url.is_empty() {
        let resolved = resolve_feed_url_offline(&xml_url);
        let resolved = if resolved.is_empty() {
            xml_url.clone()
        } else {
            resolved
        };
        // Duplicate check covers the original AND the
        // resolved URL (Python's `url IN (...)` query).
        let exists = db.feed_url_exists(&xml_url).map_err(|e| format!("{e}"))?
            || (resolved != xml_url
                && db.feed_url_exists(&resolved).map_err(|e| format!("{e}"))?);
        if !exists {
            let cat_to_use = current_category;
            if cat_to_use != UNCATEGORIZED {
                db.ensure_category_path(cat_to_use)
                    .map_err(|e| format!("{e}"))?;
            }
            db.insert_feed_if_missing(&Feed {
                id: uuid::Uuid::new_v4().to_string(),
                url: resolved,
                title: text.clone(),
                // OPML-provided labels are user-curated:
                // preserve them as custom titles so refresh
                // does not overwrite them.
                title_is_custom: !imported_title.is_empty(),
                category: Some(cat_to_use.to_string()),
                icon_url: Some(String::new()),
            })
            .map_err(|e| format!("add failed: {e}"))?;
            *added += 1;
        }
    }
    if !node.children.is_empty() {
        // A feed outline's children stay in the SAME category;
        // a folder (no xmlUrl) appends its title to the path
        // and the new path's rows are ensured.
        let new_category = if xml_url.is_empty() {
            let appended = append_category(current_category, &text);
            if appended != UNCATEGORIZED {
                db.ensure_category_path(&appended)
                    .map_err(|e| format!("{e}"))?;
            }
            appended
        } else {
            current_category.to_string()
        };
        for child in &node.children {
            import_outline(db, child, &new_category, added)?;
        }
    }
    Ok(())
}

/// Python `core/utils.py::parse_opml`: the (title, url,
/// category) triples the hosted providers' base OPML import
/// consumes. Titles are the raw text/title attributes (empty
/// when absent — no "Unknown Feed" substitution), categories
/// are full paths with the Uncategorized sentinel as the base,
/// and URLs are NOT resolved (the base import's add_feed owns
/// resolution). Err only when the document has no body.
pub fn parse_opml(bytes: &[u8]) -> Result<Vec<(String, String, String)>, String> {
    let text = decode_opml(bytes);
    let roots = parse_outline_tree(&text)?;
    let mut out = Vec::new();
    for node in &roots {
        collect_triples(node, UNCATEGORIZED, &mut out);
    }
    Ok(out)
}

fn collect_triples(
    node: &OutlineNode,
    current_category: &str,
    out: &mut Vec<(String, String, String)>,
) {
    let text = node.imported_title();
    let xml_url = node.xml_url();
    if !xml_url.is_empty() {
        out.push((text.clone(), xml_url.clone(), current_category.to_string()));
    }
    if !node.children.is_empty() {
        let new_category = if xml_url.is_empty() {
            append_category(current_category, &text)
        } else {
            current_category.to_string()
        };
        for child in &node.children {
            collect_triples(child, &new_category, out);
        }
    }
}

/// Prepare a document for Miniflux's bulk import (Python
/// `providers/miniflux.py::import_opml`): the document must
/// be STRICTLY well-formed XML (ElementTree rejects what the
/// tolerant import parse recovers — a malformed file fails
/// the Miniflux import outright), it must contain a body, and
/// when a target category is given the body's children are
/// wrapped in a single folder outline named by the category.
/// Returns the document to POST (the input, spliced, when a
/// wrap applies; Python re-serializes through ElementTree,
/// which changes bytes but not the document).
pub fn bulk_import_document(text: &str, target_category: Option<&str>) -> Result<String, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut depth = 0usize;
    let mut body_open_end: Option<u64> = None;
    let mut body_close_start: Option<u64> = None;
    let mut body_empty: Option<(u64, u64)> = None;
    let mut buf = Vec::new();
    loop {
        let pos_before = reader.buffer_position();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                if body_open_end.is_none()
                    && body_empty.is_none()
                    && e.name().as_ref().eq_ignore_ascii_case("body")
                {
                    body_open_end = Some(reader.buffer_position());
                }
            }
            Ok(Event::Empty(e)) => {
                if body_open_end.is_none()
                    && body_empty.is_none()
                    && e.name().as_ref().eq_ignore_ascii_case("body")
                {
                    // Tag spans '<' + buf + '/>'; the reader
                    // position sits just past '>'.
                    let end = reader.buffer_position();
                    body_empty = Some((end - (buf.len() as u64 + 3), end));
                }
            }
            Ok(Event::End(e)) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| "OPML document is not well-formed XML".to_string())?;
                if body_close_start.is_none() && e.name().as_ref().eq_ignore_ascii_case("body") {
                    body_close_start = Some(pos_before);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(format!("OPML document is not well-formed XML: {e}"));
            }
        }
        buf.clear();
    }
    if depth != 0 {
        return Err("OPML document is not well-formed XML".to_string());
    }
    let target = target_category.map(str::trim).filter(|t| !t.is_empty());
    if let Some((start, end)) = body_empty {
        let (start, end) = (start as usize, end as usize);
        return match target {
            Some(category) => Ok(format!(
                "{}<body><outline text=\"{}\" title=\"{}\" /></body>{}",
                &text[..start],
                escape_attr(category),
                escape_attr(category),
                &text[end..],
            )),
            None => Ok(text.to_string()),
        };
    }
    let (open_end, close_start) = match (body_open_end, body_close_start) {
        (Some(open), Some(close)) => (open as usize, close as usize),
        _ => return Err("OPML body is missing".to_string()),
    };
    match target {
        Some(category) => Ok(format!(
            "{}<outline text=\"{}\" title=\"{}\">{}</outline>{}",
            &text[..open_end],
            escape_attr(category),
            escape_attr(category),
            &text[open_end..close_start],
            &text[close_start..],
        )),
        None => Ok(text.to_string()),
    }
}

/// Attribute escaping (ElementTree's shape) for the folder
/// outline the bulk-import wrap inserts.
fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#09;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
    out
}

/// Export all subscriptions as OPML 2.0, feeds grouped by category.
pub fn export_opml(db: &Db) -> Result<String, String> {
    let feeds = db.feeds().map_err(|e| format!("{e}"))?;
    export_opml_feeds(&feeds)
}

/// Export a chosen set of feeds as OPML 2.0 with category paths
/// nested as outline hierarchies (the shape import reads back).
pub fn export_opml_feeds(feeds: &[Feed]) -> Result<String, String> {
    let mut doc = OPML {
        head: Some(opml::Head {
            title: Some("BlindRSS Subscriptions".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    #[derive(Default)]
    struct CatNode {
        feeds: Vec<Outline>,
        children: std::collections::BTreeMap<String, CatNode>,
    }
    fn node_outline(name: &str, node: &CatNode) -> Outline {
        let mut outlines: Vec<Outline> = node
            .children
            .iter()
            .map(|(child, n)| node_outline(child, n))
            .collect();
        outlines.extend(node.feeds.iter().cloned());
        Outline {
            text: name.to_string(),
            outlines,
            ..Default::default()
        }
    }
    let mut root = CatNode::default();
    for f in feeds {
        let outline = Outline {
            text: f.title.clone(),
            xml_url: Some(f.url.clone()),
            r#type: Some("rss".to_string()),
            ..Default::default()
        };
        let mut node = &mut root;
        if let Some(cat) = &f.category {
            for segment in cat
                .split(crate::db::CATEGORY_PATH_SEP)
                .filter(|s| !s.trim().is_empty())
            {
                node = node.children.entry(segment.trim().to_string()).or_default();
            }
        }
        node.feeds.push(outline);
    }
    for (name, node) in &root.children {
        doc.body.outlines.push(node_outline(name, node));
    }
    doc.body.outlines.extend(root.feeds);
    doc.to_string()
        .map_err(|e| format!("OPML write failed: {e}"))
}
