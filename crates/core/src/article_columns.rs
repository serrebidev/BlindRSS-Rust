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

/// Python truthiness for a stored `visible` value
/// (`bool(entry.get("visible", True))`): only null-when-present
/// is impossible here — a missing key defaults to true before
/// this is called; false, 0 and "" are false, everything else
/// true.
fn python_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        serde_json::Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Resolve the effective layout from a stored value (None =
/// defaults). Title is forced first and visible.
///
/// Python's `normalize_layout` shapes are all accepted
/// (Audit 8 S22): entries may be `{"key", "visible"}` objects
/// or bare key strings (a visible column), keys are trimmed
/// and matched case-insensitively, unknown keys are dropped,
/// duplicates collapse to the first occurrence, and keys the
/// stored layout lacks are appended in default order.
pub fn resolve_layout(stored: Option<&serde_json::Value>) -> Vec<ColumnLayout> {
    let mut out = defaults();
    if let Some(items) = stored.and_then(|v| v.as_array()) {
        let mut custom: Vec<ColumnLayout> = Vec::new();
        for item in items {
            // (raw key, visible, width) from either stored shape.
            let (raw_key, visible, width) = match item {
                serde_json::Value::String(s) => (s.as_str(), true, None),
                serde_json::Value::Object(_) => {
                    let Some(key) = item.get("key").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let visible = item.get("visible").map(python_truthy).unwrap_or(true);
                    let width = item.get("width").and_then(|v| v.as_i64()).map(|w| w as i32);
                    (key, visible, width)
                }
                _ => continue,
            };
            let key = raw_key.trim().to_lowercase();
            let Some(def) = COLUMNS.iter().find(|(k, _, _)| *k == key) else {
                continue;
            };
            if custom.iter().any(|c: &ColumnLayout| c.key == key) {
                continue;
            }
            custom.push(ColumnLayout {
                key,
                label: def.1.to_string(),
                width: width.unwrap_or(def.2),
                visible,
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

/// The English display label for a column key (Python
/// `label_for`); the app translates it at display time, the
/// stored key never changes (Audit 8 S10).
pub fn label_for(key: &str) -> Option<&'static str> {
    COLUMNS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, l, _)| *l)
}

/// Move `key` by `delta` rows (Python `move_key`): the pinned
/// Title never moves and nothing moves above it, so the
/// movable range is [1, len-1]. Returns the layout unchanged
/// for the pinned key, an unknown key, or a move that would
/// leave the range.
pub fn move_key(layout: &[ColumnLayout], key: &str, delta: i32) -> Vec<ColumnLayout> {
    let mut out = resolve_layout(Some(&layout_to_json(layout)));
    if key == PINNED_KEY {
        return out;
    }
    let Some(index) = out.iter().position(|c| c.key == key) else {
        return out;
    };
    // Python clamps the target into the movable range instead
    // of rejecting the move.
    let target = (index as i32 + delta).clamp(1, out.len() as i32 - 1) as usize;
    if target == index {
        return out;
    }
    let entry = out.remove(index);
    out.insert(target, entry);
    out
}

/// Show or hide one column (Python `set_visible`); the pinned
/// Title ignores hide requests.
pub fn set_visible(layout: &[ColumnLayout], key: &str, visible: bool) -> Vec<ColumnLayout> {
    let mut out = resolve_layout(Some(&layout_to_json(layout)));
    if key == PINNED_KEY {
        return out;
    }
    for entry in out.iter_mut() {
        if entry.key == key {
            entry.visible = visible;
        }
    }
    out
}

/// Per-feed layout override out of a feed's settings object
/// (Python `feed_layout_from_settings`): the `columns` value
/// absent, null, empty or not a list means inherit (None);
/// anything else normalizes like a global layout (Audit 8 S03).
pub fn feed_layout_from_settings(settings: &serde_json::Value) -> Option<Vec<ColumnLayout>> {
    let raw = settings.as_object()?.get("columns")?;
    let items = raw.as_array()?;
    if items.is_empty() {
        return None;
    }
    Some(resolve_layout(Some(raw)))
}

/// The layout a view renders with (Python `resolve_layout`):
/// the feed override when one applies, else the global layout.
pub fn resolve_for_view(
    global: &[ColumnLayout],
    feed: Option<Vec<ColumnLayout>>,
) -> Vec<ColumnLayout> {
    feed.unwrap_or_else(|| global.to_vec())
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
