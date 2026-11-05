//! Per-feed settings (`feeds.feed_settings` JSON) conversions
//! for the Feed Properties dialog and their consumers
//! (Audit 8 G08; Python `gui/dialogs.py` FeedPropertiesDialog
//! and `providers/local.py` per-feed overrides, issue #29/#75).
//!
//! The dialog edits a patch that `Db::set_feed_settings`
//! merges into the stored object — keys the dialog does not
//! show are never erased. "Inherit" is stored exactly as
//! Python stores it: the key present with a null value (or,
//! for the text overrides, an empty string).

use serde_json::{Map, Value, json};

/// Refresh-interval choices for one feed (Python
/// `_refresh_interval_choices`): the stored seconds value
/// (None = use the global setting; 0 = manual refresh only)
/// and its display label.
pub const REFRESH_INTERVAL_CHOICES: &[(Option<i64>, &str)] = &[
    (None, "Use global setting"),
    (Some(0), "Never (manual refresh only)"),
    (Some(30), "30 seconds"),
    (Some(60), "1 minute"),
    (Some(120), "2 minutes"),
    (Some(180), "3 minutes"),
    (Some(240), "4 minutes"),
    (Some(300), "5 minutes"),
    (Some(600), "10 minutes"),
    (Some(900), "15 minutes"),
    (Some(1800), "30 minutes"),
    (Some(3600), "60 minutes"),
    (Some(7200), "2 hours"),
    (Some(10800), "3 hours"),
    (Some(14400), "4 hours"),
];

/// A stored numeric setting as an integer, Python-style:
/// JSON numbers only (bools are not numbers), floats truncate.
fn stored_int(settings: &Value, key: &str) -> Option<i64> {
    match settings.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// The choice index the dialog selects for a stored
/// `refresh_interval_seconds` (Python's selection rule: an
/// exact value wins; otherwise the nearest positive choice
/// for a positive stored value; anything else is "Use global
/// setting").
pub fn refresh_interval_index(settings: &Value) -> usize {
    let Some(current) = stored_int(settings, "refresh_interval_seconds") else {
        return 0;
    };
    let current = current.max(0);
    let mut index = 0usize;
    let mut best_diff: Option<i64> = None;
    for (i, (value, _)) in REFRESH_INTERVAL_CHOICES.iter().enumerate() {
        let Some(value) = value else { continue };
        if *value == current {
            return i;
        }
        if current > 0 && *value > 0 {
            let diff = (value - current).abs();
            if best_diff.map(|b| diff < b).unwrap_or(true) {
                best_diff = Some(diff);
                index = i;
            }
        }
    }
    index
}

/// The stored interval value for a choice index (None for
/// "Use global setting" or an out-of-range index).
pub fn refresh_interval_value(index: usize) -> Option<i64> {
    REFRESH_INTERVAL_CHOICES
        .get(index)
        .and_then(|(value, _)| *value)
}

/// The effective scheduled-refresh interval for a feed
/// (Python `_filter_scheduled_due_rows`): its
/// `refresh_interval_seconds` override when set, else the
/// global interval. 0 means never auto-refresh.
pub fn effective_refresh_interval(global: i64, settings: &Value) -> i64 {
    stored_int(settings, "refresh_interval_seconds").unwrap_or(global)
}

/// Whether a scheduled refresh is due for a feed with this
/// effective interval (Python's rule, including its 1-second
/// tolerance so a feed whose interval equals the loop tick is
/// not pushed a whole extra tick): never-attempted feeds are
/// always due; a non-positive interval is never due.
pub fn scheduled_due(effective: i64, last_attempt: Option<f64>, now: f64) -> bool {
    if effective <= 0 {
        return false;
    }
    match last_attempt {
        None => true,
        Some(last) => now - last >= effective as f64 - 1.0,
    }
}

/// Custom request headers as dialog text (Python: one
/// `Name: Value` line per stored entry, in stored order).
pub fn headers_text(settings: &Value) -> String {
    let Some(map) = settings.get("custom_headers").and_then(|v| v.as_object()) else {
        return String::new();
    };
    map.iter()
        .map(|(k, v)| {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}: {value}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse the headers text back (Python `_save_feed_settings`):
/// each non-empty line with a colon contributes its trimmed
/// name and value; lines without a colon and empty names are
/// skipped.
pub fn parse_headers_text(text: &str) -> Value {
    let mut map = Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        map.insert(name.to_string(), json!(value.trim()));
    }
    Value::Object(map)
}

/// The timeout field's initial text (blank = default).
pub fn timeout_text(settings: &Value) -> String {
    stored_int(settings, "timeout_seconds")
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// Parse the timeout field (Python: an integer, blank or
/// invalid becomes None = the global timeout applies).
pub fn parse_timeout_text(text: &str) -> Option<i64> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<i64>().ok()
}

/// Impersonation modes in choice order (Python
/// `_impersonate_values`).
pub const IMPERSONATE_VALUES: [&str; 3] = ["auto", "always", "never"];
pub const IMPERSONATE_LABELS: [&str; 3] = ["Auto", "Always", "Never"];

/// A raw impersonation mode, validated (Python: anything
/// outside the three modes reads back as "auto").
pub fn normalize_impersonate(raw: &str) -> &'static str {
    let raw = raw.to_lowercase();
    IMPERSONATE_VALUES
        .iter()
        .find(|v| **v == raw)
        .copied()
        .unwrap_or("auto")
}

/// The stored impersonation mode, validated.
pub fn impersonate_value(settings: &Value) -> &'static str {
    normalize_impersonate(
        settings
            .get("impersonate")
            .and_then(|v| v.as_str())
            .unwrap_or("auto"),
    )
}

/// A stored text override ("" when unset or not a string).
pub fn text_value(settings: &Value, key: &str) -> String {
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Whether an encoding name is one the decoder knows — the
/// Rust counterpart of Python's `codecs.lookup` validation.
/// Blank (automatic detection) is valid.
pub fn valid_encoding(text: &str) -> bool {
    let value = text.trim();
    value.is_empty() || encoding_rs::Encoding::for_label(value.as_bytes()).is_some()
}

/// The settings patch the dialog saves (Python
/// `_save_feed_settings`): headers always an object, timeout
/// and interval and columns as value-or-null (null = inherit),
/// impersonation always one of the three modes, proxy and
/// encodings as trimmed strings (encodings lowercased, "" =
/// automatic). `columns` is the edited layout JSON, or None
/// when the editor is in its inherit state.
#[allow(clippy::too_many_arguments)]
pub fn build_patch(
    headers: &str,
    timeout: &str,
    impersonate: &str,
    proxy: &str,
    encoding: &str,
    fulltext_encoding: &str,
    interval_index: usize,
    columns: Option<Value>,
) -> Value {
    json!({
        "custom_headers": parse_headers_text(headers),
        "timeout_seconds": parse_timeout_text(timeout).map(Value::from).unwrap_or(Value::Null),
        "impersonate": normalize_impersonate(impersonate),
        "proxy": proxy.trim(),
        "encoding": encoding.trim().to_lowercase(),
        "fulltext_encoding": fulltext_encoding.trim().to_lowercase(),
        "refresh_interval_seconds": refresh_interval_value(interval_index).map(Value::from).unwrap_or(Value::Null),
        "columns": columns.unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_selection_exact_nearest_and_inherit() {
        assert_eq!(refresh_interval_index(&json!({})), 0);
        assert_eq!(
            refresh_interval_index(&json!({"refresh_interval_seconds": 0})),
            1
        );
        assert_eq!(
            refresh_interval_index(&json!({"refresh_interval_seconds": 300})),
            7
        );
        // Not a choice: nearest positive (290 -> 300).
        assert_eq!(
            refresh_interval_index(&json!({"refresh_interval_seconds": 290})),
            7
        );
        // Negative clamps to 0 = "Never".
        assert_eq!(
            refresh_interval_index(&json!({"refresh_interval_seconds": -5})),
            1
        );
        // A string is not a number: inherit.
        assert_eq!(
            refresh_interval_index(&json!({"refresh_interval_seconds": "300"})),
            0
        );
    }

    #[test]
    fn headers_round_trip_and_skip_junk() {
        let settings = json!({"custom_headers": {"X-A": "1", "Referer": "https://x.test/"}});
        let text = headers_text(&settings);
        assert_eq!(text, "X-A: 1\nReferer: https://x.test/");
        let parsed = parse_headers_text("X-A: 1\nno colon here\n: empty name\nB:  two \n\n");
        assert_eq!(parsed, json!({"X-A": "1", "B": "two"}));
        assert_eq!(headers_text(&json!({})), "");
    }

    #[test]
    fn patch_shape_matches_python() {
        let patch = build_patch(
            "A: b",
            "",
            "ALWAYS",
            " http://p:8080 ",
            "UTF-8",
            "",
            0,
            None,
        );
        assert_eq!(patch["custom_headers"], json!({"A": "b"}));
        assert_eq!(patch["timeout_seconds"], Value::Null);
        assert_eq!(patch["impersonate"], json!("always"));
        assert_eq!(patch["proxy"], json!("http://p:8080"));
        assert_eq!(patch["encoding"], json!("utf-8"));
        assert_eq!(patch["fulltext_encoding"], json!(""));
        assert_eq!(patch["refresh_interval_seconds"], Value::Null);
        assert_eq!(patch["columns"], Value::Null);
        let patch = build_patch("", "45", "bogus", "", "", "", 7, Some(json!([])));
        assert_eq!(patch["timeout_seconds"], json!(45));
        assert_eq!(patch["impersonate"], json!("auto"));
        assert_eq!(patch["refresh_interval_seconds"], json!(300));
    }

    #[test]
    fn scheduling_rules() {
        assert_eq!(effective_refresh_interval(300, &json!({})), 300);
        assert_eq!(
            effective_refresh_interval(300, &json!({"refresh_interval_seconds": 0})),
            0
        );
        assert!(scheduled_due(300, None, 1000.0));
        assert!(!scheduled_due(0, None, 1000.0));
        assert!(!scheduled_due(300, Some(900.0), 1000.0));
        // 1s tolerance: 299s elapsed of 300 is due.
        assert!(scheduled_due(300, Some(701.0), 1000.0));
    }
}
