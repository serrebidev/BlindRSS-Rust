//! Named retention policies (Python `core/retention.py`).
//!
//! Config stores stable identifiers ("1_week", "unlimited");
//! logic only ever sees identifiers or resolved day counts.

/// (identifier, days) in UI order; None days = keep forever.
pub const RETENTION_CHOICES: [(&str, Option<i64>); 12] = [
    ("1_day", Some(1)),
    ("3_days", Some(3)),
    ("1_week", Some(7)),
    ("2_weeks", Some(14)),
    ("3_weeks", Some(21)),
    ("1_month", Some(30)),
    ("2_months", Some(60)),
    ("6_months", Some(180)),
    ("1_year", Some(365)),
    ("2_years", Some(730)),
    ("5_years", Some(1825)),
    ("unlimited", None),
];

pub const RETENTION_DEFAULT: &str = "unlimited";

/// Normalize a stored value to a known identifier. Python also
/// honors two hidden legacy ids ("2_days", "3_months") and the
/// old display labels ("2 days", ...).
pub fn normalize_retention(value: &str) -> String {
    let v = value.trim().to_lowercase().replace(' ', "_");
    match v.as_str() {
        "2_days" | "3_months" => return v,
        _ => {}
    }
    if RETENTION_CHOICES.iter().any(|(id, _)| *id == v) {
        v
    } else {
        RETENTION_DEFAULT.to_string()
    }
}

/// Days kept for an identifier, None = forever.
pub fn retention_days(value: &str) -> Option<i64> {
    let id = normalize_retention(value);
    match id.as_str() {
        "2_days" => Some(2),
        "3_months" => Some(90),
        _ => RETENTION_CHOICES
            .iter()
            .find(|(cid, _)| *cid == id)
            .and_then(|(_, days)| *days),
    }
}

/// Articles whose date could not be parsed carry the sentinel
/// "0001-01-01 00:00:00" (or an empty string). Those compare below
/// every real cutoff, so retention treats them as undated rather
/// than ancient (Python `core/db.py` UNDATED_ARTICLE_FLOOR).
pub const UNDATED_ARTICLE_FLOOR: &str = "0002";

/// Format epoch seconds as a UTC `YYYY-MM-DD` date.
fn ymd_from_epoch_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Days since epoch -> (y, m, d) via the civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The `YYYY-MM-DD` boundary retention compares article dates
/// against (Python `retention_cutoff_date`). None means "keep
/// forever". This is the single definition shared by the cleanup
/// sweep and by refresh (Python issue #106): a feed keeps serving
/// entries retention has already purged, so refresh must recognize
/// the same boundary or it re-inserts them as unread.
pub fn retention_cutoff_date(days: Option<i64>) -> Option<String> {
    let days = days?;
    if days < 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(ymd_from_epoch_secs(now - days * 86_400))
}

/// True when an article's date puts it beyond the retention window
/// (Python `article_is_outside_retention`). Mirrors the cleanup
/// sweep's WHERE clause exactly, including the undated guard, so
/// refresh never inserts a row the next sweep would delete again.
pub fn article_is_outside_retention(date_value: Option<&str>, cutoff: Option<&str>) -> bool {
    let Some(cutoff) = cutoff else {
        return false;
    };
    let stored = date_value.unwrap_or("").trim();
    if stored.is_empty() || stored < UNDATED_ARTICLE_FLOOR {
        return false;
    }
    stored < cutoff
}

/// Display label for an identifier.
pub fn retention_label(id: &str) -> String {
    match normalize_retention(id).as_str() {
        "1_day" => "1 day".to_string(),
        "3_days" => "3 days".to_string(),
        "2_days" => "2 days".to_string(),
        "1_week" => "1 week".to_string(),
        "2_weeks" => "2 weeks".to_string(),
        "3_weeks" => "3 weeks".to_string(),
        "1_month" => "1 month".to_string(),
        "2_months" => "2 months".to_string(),
        "3_months" => "3 months".to_string(),
        "6_months" => "6 months".to_string(),
        "1_year" => "1 year".to_string(),
        "2_years" => "2 years".to_string(),
        "5_years" => "5 years".to_string(),
        _ => "Unlimited".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_retention_matches_python() {
        let cutoff = Some("2026-10-24");
        assert!(article_is_outside_retention(
            Some("2020-01-01 00:00:00"),
            cutoff
        ));
        assert!(article_is_outside_retention(Some("2026-10-23"), cutoff));
        assert!(!article_is_outside_retention(Some("2026-10-24"), cutoff));
        assert!(!article_is_outside_retention(
            Some("2026-10-25 10:00:00"),
            cutoff
        ));
        // Undated rows are never outside, however old the sentinel looks.
        assert!(!article_is_outside_retention(
            Some("0001-01-01 00:00:00"),
            cutoff
        ));
        assert!(!article_is_outside_retention(Some(""), cutoff));
        assert!(!article_is_outside_retention(None, cutoff));
        // No cutoff = keep forever.
        assert!(!article_is_outside_retention(Some("2020-01-01"), None));
        assert_eq!(retention_cutoff_date(None), None);
        assert_eq!(retention_cutoff_date(Some(-1)), None);
        let c = retention_cutoff_date(Some(7)).unwrap();
        assert_eq!(c.len(), 10);
    }

    #[test]
    fn retention_resolution() {
        assert_eq!(retention_days("1_week"), Some(7));
        assert_eq!(retention_days("unlimited"), None);
        assert_eq!(retention_days("bogus"), None);
        assert_eq!(retention_days("3 months"), Some(90));
        assert_eq!(normalize_retention("2 Days"), "2_days");
    }
}
