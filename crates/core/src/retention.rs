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
    fn retention_resolution() {
        assert_eq!(retention_days("1_week"), Some(7));
        assert_eq!(retention_days("unlimited"), None);
        assert_eq!(retention_days("bogus"), None);
        assert_eq!(retention_days("3 months"), Some(90));
        assert_eq!(normalize_retention("2 Days"), "2_days");
    }
}
