//! The release changelog (CHANGELOG.md at the repo root),
//! parsed for two readers: the in-app Help > View Changelog
//! viewer, which shows the whole history offline from the
//! copy embedded in the binary, and `notes_between`, the
//! range-notes helper for showing every version skipped by
//! an update (Brandon's report, 2026-11-10: updating from
//! 0.1.85 to 0.1.88 showed only 0.1.88's notes — the 0.1.86
//! and 0.1.87 changes were nowhere to be found).
//!
//! The file's shape is a release contract (see the header
//! of CHANGELOG.md itself): sections headed exactly
//! `## X.Y.Z` (bare version, no `v`), newest first, with
//! range headings such as `## 0.1.2-0.1.3` when adjacent
//! releases shipped as one fix stream. The release tooling
//! extracts one section per release from the same file, so
//! this parser must agree with it: a section runs from its
//! heading to the next `## ` heading or the end of file.

/// One changelog section: the version heading as written
/// ("0.1.88", or a range like "0.1.2-0.1.3") and the
/// section body with surrounding blank lines trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogSection {
    pub version: String,
    pub body: String,
}

/// Parse the changelog into its sections, in file order
/// (newest first). Text before the first `## ` heading —
/// the file title and its intro — is not a section.
pub fn parse_changelog(markdown: &str) -> Vec<ChangelogSection> {
    let mut sections: Vec<ChangelogSection> = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some((version, body)) = current.take() {
                sections.push(ChangelogSection {
                    version,
                    body: body.join("\n").trim().to_string(),
                });
            }
            current = Some((rest.trim().to_string(), Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((version, body)) = current.take() {
        sections.push(ChangelogSection {
            version,
            body: body.join("\n").trim().to_string(),
        });
    }
    sections
}

/// A parsed X.Y.Z version, compared as a tuple.
type Version = (u32, u32, u32);

/// A version span as a comparable pair: a single version is
/// (v, v); a range heading "a-b" is (a, b) with a the older
/// end. None when either end is not a numeric X.Y.Z version.
fn version_span(version: &str) -> Option<(Version, Version)> {
    fn one(v: &str) -> Option<Version> {
        let mut parts = v.trim().split('.');
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next()?.parse().ok()?;
        let patch: u32 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some((major, minor, patch))
    }
    match version.split_once('-') {
        Some((older, newer)) => Some((one(older)?, one(newer)?)),
        None => {
            let v = one(version)?;
            Some((v, v))
        }
    }
}

/// The whole history as continuous readable plain text for
/// the changelog viewer: a "BlindRSS version X" heading per
/// section, its bullets with the "- " markers kept, and a
/// blank line between sections.
pub fn render_changelog_text(sections: &[ChangelogSection]) -> String {
    let mut out = String::new();
    for section in sections {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("BlindRSS version {}", section.version));
        if !section.body.is_empty() {
            out.push_str("\n\n");
            out.push_str(&section.body);
        }
    }
    out
}

/// The notes for every version newer than `current`, up to
/// and including `latest`, newest first, in the viewer's
/// rendered shape. This is the text an update prompt should
/// show when the user skips versions, so nothing they are
/// jumping over is hidden. Empty when nothing in the file
/// falls in the range (unknown versions included).
pub fn notes_between(sections: &[ChangelogSection], current: &str, latest: &str) -> String {
    let Some(current) = version_span(current).map(|(_, hi)| hi) else {
        return String::new();
    };
    let Some(latest) = version_span(latest).map(|(_, hi)| hi) else {
        return String::new();
    };
    if latest <= current {
        return String::new();
    }
    let in_range: Vec<ChangelogSection> = sections
        .iter()
        .filter(|s| match version_span(&s.version) {
            // A section belongs when its newest end is past
            // the running version and its oldest end is not
            // past the target: fully or partly inside the
            // skipped range.
            Some((lo, hi)) => hi > current && lo <= latest,
            None => false,
        })
        .cloned()
        .collect();
    render_changelog_text(&in_range)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Changelog

Intro text that is not a section.

## 0.1.88

- Newest fix.

## 0.1.87

- Middle fix.
- Second bullet.

## 0.1.2-0.1.3

- A combined stream.

## 0.1.1

- Oldest fix.
";

    #[test]
    fn parse_keeps_order_and_bodies() {
        let sections = parse_changelog(SAMPLE);
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].version, "0.1.88");
        assert_eq!(sections[0].body, "- Newest fix.");
        assert_eq!(sections[1].body, "- Middle fix.\n- Second bullet.");
        assert_eq!(sections[2].version, "0.1.2-0.1.3");
        assert_eq!(sections[3].version, "0.1.1");
    }

    #[test]
    fn render_heads_each_section_with_its_version() {
        let text = render_changelog_text(&parse_changelog(SAMPLE));
        assert!(text.starts_with("BlindRSS version 0.1.88\n\n- Newest fix."));
        assert!(text.contains("\n\nBlindRSS version 0.1.2-0.1.3\n\n- A combined stream."));
        assert!(text.ends_with("- Oldest fix."));
    }

    #[test]
    fn notes_between_covers_every_skipped_version() {
        // Brandon's case: 0.1.85 -> 0.1.88 must include the
        // 0.1.87 and 0.1.86 sections, not just 0.1.88's.
        let text = notes_between(&parse_changelog(SAMPLE), "0.1.85", "0.1.88");
        assert!(text.contains("BlindRSS version 0.1.88"));
        assert!(text.contains("BlindRSS version 0.1.87"));
        assert!(text.contains("- Middle fix."));
        assert!(!text.contains("0.1.2-0.1.3"));
    }

    #[test]
    fn notes_between_includes_a_range_section_it_lands_in() {
        let text = notes_between(&parse_changelog(SAMPLE), "0.1.1", "0.1.3");
        assert!(text.contains("BlindRSS version 0.1.2-0.1.3"));
        assert!(!text.contains("BlindRSS version 0.1.88"));
    }

    #[test]
    fn notes_between_is_empty_when_current_or_unknown() {
        let sections = parse_changelog(SAMPLE);
        assert_eq!(notes_between(&sections, "0.1.88", "0.1.88"), "");
        assert_eq!(notes_between(&sections, "dev-build", "0.1.88"), "");
        assert_eq!(notes_between(&sections, "0.1.85", "0.1.84"), "");
    }
}
