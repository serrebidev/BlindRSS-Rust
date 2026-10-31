//! Configurable article-list columns (Python
//! `core/article_columns.py`).
//!
//! The layout is a list of {key, visible, width} stored in
//! config `article_columns`. Title is the pinned first
//! column. Keys a saved layout does not know are dropped;
//! keys it lacks are appended in default order.

/// (key, label, default width) in default display order.
pub const COLUMNS: [(&str, &str, i32); 7] = [
    ("title", "Title", 320),
    ("author", "Author", 110),
    ("media", "Media", 110),
    ("date", "Date", 120),
    ("feed", "Feed", 140),
    ("description", "Description", 260),
    ("status", "Status", 80),
];

pub const PINNED_KEY: &str = "title";

/// One resolved layout row.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    pub key: String,
    pub label: String,
    pub width: i32,
    pub visible: bool,
}

fn defaults() -> Vec<ColumnLayout> {
    COLUMNS
        .iter()
        .map(|(k, l, w)| ColumnLayout {
            key: k.to_string(),
            label: l.to_string(),
            width: *w,
            visible: true,
        })
        .collect()
}

/// Resolve the effective layout from a stored value (None =
/// defaults). Title is forced first and visible.
pub fn resolve_layout(stored: Option<&serde_json::Value>) -> Vec<ColumnLayout> {
    let mut out = defaults();
    if let Some(items) = stored.and_then(|v| v.as_array()) {
        let mut custom: Vec<ColumnLayout> = Vec::new();
        for item in items {
            let Some(key) = item.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(def) = COLUMNS.iter().find(|(k, _, _)| *k == key) else {
                continue;
            };
            if custom.iter().any(|c: &ColumnLayout| c.key == key) {
                continue;
            }
            custom.push(ColumnLayout {
                key: key.to_string(),
                label: def.1.to_string(),
                width: item
                    .get("width")
                    .and_then(|v| v.as_i64())
                    .map(|w| w as i32)
                    .unwrap_or(def.2),
                visible: item
                    .get("visible")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            });
        }
        // Append any columns the saved layout predates.
        for def in defaults() {
            if !custom.iter().any(|c| c.key == def.key) {
                custom.push(def);
            }
        }
        out = custom;
    }
    // Title pinned first + always visible.
    if let Some(pos) = out.iter().position(|c| c.key == PINNED_KEY) {
        let mut title = out.remove(pos);
        title.visible = true;
        out.insert(0, title);
    }
    out
}

/// The layout from the app config.
pub fn layout_from_config(cfg: &crate::config::Config) -> Vec<ColumnLayout> {
    resolve_layout(cfg.raw().get("article_columns"))
}

/// Serialize a layout back to the config shape.
pub fn layout_to_json(layout: &[ColumnLayout]) -> serde_json::Value {
    serde_json::Value::Array(
        layout
            .iter()
            .map(|c| {
                serde_json::json!({
                    "key": c.key,
                    "visible": c.visible,
                    "width": c.width,
                })
            })
            .collect(),
    )
}

/// The visible columns, in order — what the list renders.
pub fn visible_columns(layout: &[ColumnLayout]) -> Vec<&ColumnLayout> {
    layout.iter().filter(|c| c.visible).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_resolution() {
        let stored = serde_json::json!([
            {"key": "date", "visible": true, "width": 99},
            {"key": "title", "visible": false},
            {"key": "bogus", "visible": true},
        ]);
        let layout = resolve_layout(Some(&stored));
        assert_eq!(layout[0].key, "title");
        assert!(layout[0].visible);
        assert_eq!(layout[1].key, "date");
        assert_eq!(layout[1].width, 99);
        assert!(!layout.iter().any(|c| c.key == "bogus"));
        assert_eq!(layout.len(), 7);
        assert_eq!(resolve_layout(None).len(), 7);
    }
}
