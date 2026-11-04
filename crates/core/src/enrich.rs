//! Metadata enrichment (Python `core/metadata_enrich.py`).
//!
//! Many feeds omit the author and the site's tags/categories
//! even though the article page declares them. After a page
//! is fetched for full text, its declared metadata merges
//! into the stored article row. This module is a faithful
//! port of the Python module, in its exact source order:
//!
//! 1. JSON-LD whose `@type` is an Article-family type,
//! 2. microdata items of an Article-family type,
//! 3. OpenGraph `article:*` properties,
//! 4. the trafilatura metadata fallback.
//!
//! Earlier sources win per field; tags union across sources.
//! Extraction-side cleaning dedupes Unicode-case-insensitively
//! and caps the extracted list (Python `_MAX_TAGS`), but the
//! stored union is lossless: `merge_tag_string` preserves
//! every existing tag and caps only additions. Enrichment
//! never issues its own network requests — it reads only the
//! page HTML the app already downloaded.

use serde_json::Value;

/// What a page declares about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnrichMeta {
    pub author: String,
    pub tags: Vec<String>,
    pub section: String,
}

/// Python `_MAX_TAGS`: cap on one extracted keyword list and
/// on the merged extraction result.
pub const MAX_TAGS: usize = 20;
/// Python `_MAX_TAG_LEN`, in Unicode code points.
pub const MAX_TAG_LEN: usize = 80;
/// `merge_tag_string` appends additions only while the stored
/// list is shorter than `_MAX_TAGS * 2`.
const MAX_STORED_TAGS: usize = MAX_TAGS * 2;

/// Python `_ARTICLE_TYPES` (matched case-sensitively).
const ARTICLE_TYPES: [&str; 9] = [
    "Article",
    "NewsArticle",
    "BlogPosting",
    "Report",
    "ScholarlyArticle",
    "SocialMediaPosting",
    "TechArticle",
    "LiveBlogPosting",
    "ReportageNewsArticle",
];

fn clean(value: &str) -> String {
    value.trim().to_string()
}

/// Python `_clean` on a JSON value: strings pass through,
/// other scalars stringify, containers/null become "".
fn clean_value(value: &Value) -> String {
    match value {
        Value::String(s) => clean(s),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// Python `_person_name`: a string, a `{"name": ...}` object,
/// or a list whose first three named people join with ", ".
fn person_name(value: &Value) -> String {
    match value {
        Value::String(_) => clean_value(value),
        Value::Object(map) => map.get("name").map_or(String::new(), clean_value),
        Value::Array(items) => items
            .iter()
            .map(person_name)
            .filter(|n| !n.is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Python `_keyword_list`: comma string or list of strings /
/// `{"name": ...}` objects; overlong entries dropped, Unicode
/// case-insensitive dedupe, capped at `_MAX_TAGS`.
fn keyword_list(value: &Value) -> Vec<String> {
    let raw: Vec<String> = match value {
        Value::String(s) => s.split(',').map(clean).collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(_) => Some(clean_value(item)),
                Value::Object(map) => map.get("name").map(clean_value),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        if tag.is_empty() || tag.chars().count() > MAX_TAG_LEN {
            continue;
        }
        if out.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
            continue;
        }
        out.push(tag);
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// `articleSection`: a list joins its keyword list with
/// ", "; anything else is a cleaned scalar.
fn section_value(value: &Value) -> String {
    match value {
        Value::Array(_) => keyword_list(value).join(", "),
        _ => clean_value(value),
    }
}

fn meta_from_node(node: &Value) -> EnrichMeta {
    let empty = Value::Null;
    EnrichMeta {
        author: person_name(node.get("author").unwrap_or(&empty)),
        tags: keyword_list(node.get("keywords").unwrap_or(&empty)),
        section: section_value(node.get("articleSection").unwrap_or(&empty)),
    }
}

fn is_article_type(value: &Value) -> bool {
    match value {
        Value::String(s) => ARTICLE_TYPES.contains(&s.as_str()),
        Value::Array(items) => items
            .iter()
            .any(|i| i.as_str().is_some_and(|s| ARTICLE_TYPES.contains(&s))),
        _ => false,
    }
}

/// Python `_merge_meta`: first non-empty author/section win;
/// tags union in order with Unicode case-insensitive dedupe,
/// capped at `_MAX_TAGS`.
fn merge_meta(base: &mut EnrichMeta, extra: EnrichMeta) {
    if base.author.is_empty() {
        base.author = extra.author;
    }
    if base.section.is_empty() {
        base.section = extra.section;
    }
    for tag in extra.tags {
        if base.tags.len() >= MAX_TAGS {
            break;
        }
        if !base
            .tags
            .iter()
            .any(|t| t.to_lowercase() == tag.to_lowercase())
        {
            base.tags.push(tag);
        }
    }
}

fn meta_complete(meta: &EnrichMeta) -> bool {
    !meta.author.is_empty() && !meta.tags.is_empty() && !meta.section.is_empty()
}

/// Extract enrichment metadata from a page's HTML, following
/// Python's source order exactly (see the module docs). The
/// `url` is the page address: microdata URL properties and
/// the fallback resolve against it. Never fetches anything.
pub fn extract_metadata(html: &str, url: &str) -> EnrichMeta {
    let mut meta = EnrichMeta::default();
    if html.trim().is_empty() {
        return meta;
    }
    if let Some(found) = from_jsonld(html) {
        merge_meta(&mut meta, found);
    }
    let document = if meta_complete(&meta) {
        None
    } else {
        Some(rust_trafilatura::parse_html(html))
    };
    if let Some(document) = &document {
        if !meta_complete(&meta)
            && let Some(found) = from_microdata(document, url)
        {
            merge_meta(&mut meta, found);
        }
        if !meta_complete(&meta)
            && let Some(found) = from_opengraph(document)
        {
            merge_meta(&mut meta, found);
        }
        if !meta_complete(&meta) {
            merge_meta(&mut meta, from_trafilatura(document));
        }
    }
    meta
}

// ----------------------------------------------------------------
// JSON-LD (Python `_from_jsonld` over extruct's json-ld output)

/// The text of every `<script type="application/ld+json">`
/// block, in document order.
fn jsonld_blocks(html: &str) -> Vec<String> {
    let lower = html.to_lowercase();
    let mut blocks = Vec::new();
    let mut search = 0usize;
    while let Some(pos) = lower[search..].find("<script") {
        let start = search + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let tag = &lower[start..start + gt];
        if tag.contains("application/ld+json") {
            let content_start = start + gt + 1;
            let Some(end) = lower[content_start..].find("</script>") else {
                break;
            };
            blocks.push(html[content_start..content_start + end].to_string());
            search = content_start + end;
        } else {
            search = start + gt + 1;
        }
    }
    blocks
}

/// Python `_jsonld_nodes`: breadth-first flattening; `@graph`
/// members queue behind the remaining top-level nodes.
fn jsonld_nodes(data: &Value) -> Vec<&Value> {
    let mut queue: Vec<&Value> = match data {
        Value::Array(items) => items.iter().collect(),
        _ => vec![data],
    };
    let mut out = Vec::new();
    let mut index = 0;
    while index < queue.len() {
        let node = queue[index];
        index += 1;
        if let Value::Object(map) = node {
            out.push(node);
            if let Some(Value::Array(graph)) = map.get("@graph") {
                queue.extend(graph.iter());
            }
        }
    }
    out
}

/// The first Article-family JSON-LD node's metadata, if any
/// (returned even when every field is empty, like Python).
fn from_jsonld(html: &str) -> Option<EnrichMeta> {
    for block in jsonld_blocks(html) {
        let Ok(data) = serde_json::from_str::<Value>(&block) else {
            continue;
        };
        for node in jsonld_nodes(&data) {
            if node.get("@type").is_some_and(is_article_type) {
                return Some(meta_from_node(node));
            }
        }
    }
    None
}

// ----------------------------------------------------------------
// Microdata (Python `_from_microdata` over extruct's output)

use rust_trafilatura::{Document, Kind, NodeId};

fn node_text(document: &Document, id: NodeId) -> String {
    let node = &document.nodes[id];
    if node.kind == Kind::Text {
        return node.data.clone();
    }
    let mut out = String::new();
    for child in &node.children {
        out.push_str(&node_text(document, *child));
    }
    out
}

fn resolve_url(base: &str, value: &str) -> String {
    if base.is_empty() {
        return value.to_string();
    }
    url::Url::parse(base)
        .and_then(|b| b.join(value))
        .map_or_else(|_| value.to_string(), |u| u.to_string())
}

/// The microdata property value of an element, per the spec
/// extruct implements: which attribute (or the text) counts
/// depends on the element name.
fn property_value(document: &Document, id: NodeId, base: &str) -> Value {
    let node = &document.nodes[id];
    let raw = match node.tag.as_str() {
        "meta" => node.attr("content").to_string(),
        "audio" | "embed" | "iframe" | "img" | "source" | "track" | "video" => {
            resolve_url(base, node.attr("src"))
        }
        "a" | "area" | "link" => resolve_url(base, node.attr("href")),
        "object" => resolve_url(base, node.attr("data")),
        "data" | "meter" => node.attr("value").to_string(),
        "time" if !node.attr("datetime").is_empty() => node.attr("datetime").to_string(),
        _ => node_text(document, id).trim().to_string(),
    };
    Value::String(raw)
}

/// One microdata item as an extruct-shaped object:
/// `{"type": ..., "properties": {name: value | [values]}}`.
fn item_value(document: &Document, id: NodeId, base: &str) -> Value {
    let mut properties = serde_json::Map::new();
    let mut collected: Vec<(String, Value)> = Vec::new();
    collect_properties(document, id, base, &mut collected);
    for (name, value) in collected {
        match properties.get_mut(&name) {
            Some(Value::Array(existing)) => existing.push(value),
            Some(existing) => {
                let first = existing.take();
                properties.insert(name, Value::Array(vec![first, value]));
            }
            None => {
                properties.insert(name, value);
            }
        }
    }
    let itemtype = document.nodes[id]
        .attr("itemtype")
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    let mut item = serde_json::Map::new();
    item.insert("type".to_string(), Value::String(itemtype));
    item.insert("properties".to_string(), Value::Object(properties));
    Value::Object(item)
}

/// Collect an item's properties from its subtree: descend
/// until a nested `itemscope` boundary; a nested item that is
/// itself a property becomes the (nested item) value and its
/// subtree is not crawled for the parent's properties.
fn collect_properties(
    document: &Document,
    item: NodeId,
    base: &str,
    out: &mut Vec<(String, Value)>,
) {
    fn walk(document: &Document, id: NodeId, base: &str, out: &mut Vec<(String, Value)>) {
        for child in &document.nodes[id].children {
            let child = *child;
            let node = &document.nodes[child];
            if node.kind != Kind::Element {
                continue;
            }
            let has_scope = node.has_attr("itemscope");
            let prop = node.attr("itemprop");
            if has_scope {
                if !prop.is_empty() {
                    let value = item_value(document, child, base);
                    for name in prop.split_whitespace() {
                        out.push((name.to_string(), value.clone()));
                    }
                }
                continue;
            }
            if !prop.is_empty() {
                let value = property_value(document, child, base);
                for name in prop.split_whitespace() {
                    out.push((name.to_string(), value.clone()));
                }
            }
            walk(document, child, base, out);
        }
    }
    walk(document, item, base, out);
}

/// Every item extruct would list, in document order: an
/// `itemscope` element that is not the property value of
/// another item (an `itemprop` element inside an item is a
/// value, not a listed item).
fn top_level_items(document: &Document) -> Vec<NodeId> {
    fn walk(document: &Document, id: NodeId, inside_item: bool, out: &mut Vec<NodeId>) {
        for child in &document.nodes[id].children {
            let child = *child;
            let node = &document.nodes[child];
            if node.kind != Kind::Element {
                continue;
            }
            let has_scope = node.has_attr("itemscope");
            if has_scope && !(inside_item && node.has_attr("itemprop")) {
                out.push(child);
            }
            walk(document, child, inside_item || has_scope, out);
        }
    }
    let mut out = Vec::new();
    walk(document, 0, false, &mut out);
    out
}

fn from_microdata(document: &Document, base: &str) -> Option<EnrichMeta> {
    let empty = Value::Null;
    for item_id in top_level_items(document) {
        let item = item_value(document, item_id, base);
        let itemtype = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or("");
        if !ARTICLE_TYPES.contains(&itemtype) {
            continue;
        }
        let properties = item.get("properties").unwrap_or(&empty);
        return Some(EnrichMeta {
            author: person_name(properties.get("author").unwrap_or(&empty)),
            tags: keyword_list(properties.get("keywords").unwrap_or(&empty)),
            section: section_value(properties.get("articleSection").unwrap_or(&empty)),
        });
    }
    None
}

// ----------------------------------------------------------------
// OpenGraph (Python `_from_opengraph` over extruct's output)

fn from_opengraph(document: &Document) -> Option<EnrichMeta> {
    let mut author = String::new();
    let mut tags: Vec<String> = Vec::new();
    let mut section = String::new();
    for id in document.tagged(0, "meta") {
        let node = &document.nodes[id];
        let content = node.attr("content");
        match node.attr("property") {
            // A profile URL is never a byline (Python rule).
            "article:author" if author.is_empty() => {
                let value = clean(content);
                if !value.is_empty() && !value.contains("://") {
                    author = value;
                }
            }
            "article:tag" => {
                let value = clean(content);
                if !value.is_empty()
                    && value.chars().count() <= MAX_TAG_LEN
                    && !tags
                        .iter()
                        .any(|t| t.to_lowercase() == value.to_lowercase())
                {
                    tags.push(value);
                }
            }
            "article:section" if section.is_empty() => {
                section = clean(content);
            }
            _ => {}
        }
    }
    if author.is_empty() && tags.is_empty() && section.is_empty() {
        return None;
    }
    tags.truncate(MAX_TAGS);
    Some(EnrichMeta {
        author,
        tags,
        section,
    })
}

// ----------------------------------------------------------------
// Trafilatura fallback (Python's `trafilatura.extract_metadata`)

fn from_trafilatura(document: &Document) -> EnrichMeta {
    let options = rust_trafilatura::Options::default();
    let found = rust_trafilatura::metadata::extract_metadata(document, &options);
    let mut combined: Vec<String> = found.tags;
    combined.extend(found.categories);
    EnrichMeta {
        author: clean(&found.author),
        tags: keyword_list(&Value::Array(
            combined.into_iter().map(Value::String).collect(),
        )),
        section: String::new(),
    }
}

// ----------------------------------------------------------------
// Stored-tag merge (Python `merge_tag_string`)

/// Merge new tags into the stored newline-separated tag list
/// the lossless way: every existing tag survives (cleaned,
/// Unicode case-insensitively deduped, no length or count
/// cap on what is already stored); additions are appended
/// only while the total stays below `_MAX_TAGS * 2` (40).
pub fn merge_tag_string(existing: &[String], new_tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in existing {
        let tag = clean(tag);
        if tag.is_empty() {
            continue;
        }
        if out.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
            continue;
        }
        out.push(tag);
    }
    for tag in new_tags {
        if out.len() >= MAX_STORED_TAGS {
            break;
        }
        let tag = clean(tag);
        if tag.is_empty() {
            continue;
        }
        if out.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
            continue;
        }
        out.push(tag);
    }
    out
}

/// Python `_author_is_placeholder`: enrichment may replace
/// only an empty or "unknown" byline, never a real one.
pub fn author_is_placeholder(author: &str) -> bool {
    matches!(
        clean(author).to_lowercase().as_str(),
        "" | "unknown" | "(unknown)"
    )
}

/// Feed-ingest tag merge (NOT the enrichment path): first
/// spelling wins, ASCII case-insensitive dedupe, per-tag
/// length cap, overall cap. Feed categories keep this older
/// behaviour; the lossless stored union above is specific
/// to page-metadata enrichment.
pub fn merge_tags(existing: Vec<String>, extra: &[String]) -> Vec<String> {
    const FEED_MAX_TAGS: usize = 12;
    let mut out: Vec<String> = Vec::new();
    for tag in existing.iter().chain(extra.iter()) {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().count() > MAX_TAG_LEN {
            continue;
        }
        if out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            continue;
        }
        out.push(tag.to_string());
        if out.len() >= FEED_MAX_TAGS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonld_wins_over_generic_meta() {
        // Python priority: JSON-LD first; the generic author
        // meta and news_keywords are fallback-only material,
        // and metadata_enrich never strips a "By " prefix.
        let html = r#"<html><head>
            <meta name="author" content="By Jane Doe">
            <meta property="article:section" content="Technology">
            <meta name="news_keywords" content="AI, Chips, AI">
            <script type="application/ld+json">{"@type":"NewsArticle","keywords":["Rust","Chips"],"author":{"name":"John Smith"}}</script>
            </head><body></body></html>"#;
        let meta = extract_metadata(html, "https://example.com/story");
        assert_eq!(meta.author, "John Smith");
        assert_eq!(meta.tags, vec!["Rust", "Chips"]);
        // Section was still missing after JSON-LD, so the
        // OpenGraph property fills it.
        assert_eq!(meta.section, "Technology");
    }

    #[test]
    fn merge_tag_string_caps_only_additions() {
        let existing: Vec<String> = (0..25).map(|i| format!("t{i}")).collect();
        let merged = merge_tag_string(&existing, &["New".to_string()]);
        assert_eq!(merged.len(), 26);
        let existing: Vec<String> = (0..45).map(|i| format!("t{i}")).collect();
        let merged = merge_tag_string(&existing, &["New".to_string()]);
        assert_eq!(merged.len(), 45);
    }
}
