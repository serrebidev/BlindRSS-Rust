//! Metadata enrichment (Python `core/metadata_enrich.py`).
//!
//! Many feeds omit the author and the site's tags/categories
//! even though the article page declares them. After a page
//! is fetched, its declared author / tags / section merge
//! into the stored article row: a missing author is filled,
//! tags merge case-insensitively (deduplicated, capped), and
//! the section is kept with the tags. This is where the
//! filter-rule Tag field gets its data.

/// What a page declares about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnrichMeta {
    pub author: String,
    pub tags: Vec<String>,
    pub section: String,
}

const MAX_TAGS: usize = 12;
const MAX_TAG_LEN: usize = 80;

/// Extract enrichment metadata from a page's HTML: meta tags
/// (author, article:tag, article:section, news_keywords) and
/// JSON-LD (author name, keywords).
pub fn extract_metadata(html: &str) -> EnrichMeta {
    let mut meta = EnrichMeta::default();
    if let Some(a) = crate::extract::meta_content(html, "author") {
        let a = clean_author(&a);
        if !a.is_empty() {
            meta.author = a;
        }
    }
    if meta.author.is_empty()
        && let Some(a) = crate::extract::meta_content(html, "article:author")
    {
        // article:author often holds a profile URL, never a byline.
        if !a.trim().starts_with("http") {
            meta.author = clean_author(&a);
        }
    }
    if let Some(s) = crate::extract::meta_content(html, "article:section") {
        meta.section = s.trim().to_string();
    }
    let mut raw_tags: Vec<String> = Vec::new();
    for name in ["article:tag", "news_keywords", "keywords"] {
        if let Some(v) = crate::extract::meta_content(html, name) {
            raw_tags.extend(split_keywords(&v));
        }
    }
    // JSON-LD blocks: keywords + author.name.
    let mut rest = html;
    while let Some(pos) = rest.find("application/ld+json") {
        let after = &rest[pos..];
        let (Some(start), Some(end)) = (after.find('>'), after.find("</script>")) else {
            break;
        };
        let raw = &after[start + 1..end];
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
            collect_jsonld(&value, &mut raw_tags, &mut meta);
        }
        rest = &after[end..];
    }
    meta.tags = merge_tags(Vec::new(), &raw_tags);
    if !meta.section.is_empty()
        && !meta
            .tags
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&meta.section))
    {
        meta.tags.push(meta.section.clone());
    }
    meta
}

fn clean_author(raw: &str) -> String {
    let a = raw.trim();
    let a = a.strip_prefix("By ").unwrap_or(a);
    a.trim().to_string()
}

fn split_keywords(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn collect_jsonld(value: &serde_json::Value, tags: &mut Vec<String>, meta: &mut EnrichMeta) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match k.as_str() {
                    "keywords" => match v {
                        serde_json::Value::String(s) => tags.extend(split_keywords(s)),
                        serde_json::Value::Array(items) => {
                            for item in items {
                                if let Some(s) = item.as_str() {
                                    tags.push(s.trim().to_string());
                                }
                            }
                        }
                        _ => {}
                    },
                    "author" if meta.author.is_empty() => {
                        if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                            meta.author = clean_author(name);
                        } else if let Some(name) = v.as_str() {
                            meta.author = clean_author(name);
                        }
                    }
                    _ => collect_jsonld(v, tags, meta),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_jsonld(item, tags, meta);
            }
        }
        _ => {}
    }
}

/// Merge tag lists the Python way: case-insensitive dedupe,
/// first spelling wins, per-tag length cap, overall cap.
pub fn merge_tags(existing: Vec<String>, extra: &[String]) -> Vec<String> {
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
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_extraction() {
        let html = r#"<html><head>
            <meta name="author" content="By Jane Doe">
            <meta property="article:section" content="Technology">
            <meta name="news_keywords" content="AI, Chips, AI">
            <script type="application/ld+json">{"@type":"NewsArticle","keywords":["Rust","Chips"],"author":{"name":"John Smith"}}</script>
            </head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.author, "Jane Doe");
        assert_eq!(meta.section, "Technology");
        assert!(meta.tags.iter().any(|t| t == "AI"));
        assert!(meta.tags.iter().any(|t| t == "Rust"));
        assert_eq!(
            meta.tags
                .iter()
                .filter(|t| t.eq_ignore_ascii_case("chips"))
                .count(),
            1
        );
    }
}
