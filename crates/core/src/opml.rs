//! OPML import/export (Stage 2, slice 3).
//!
//! Category nesting follows the common convention Python also reads:
//! an outline without xmlUrl is a category; feed outlines inside it
//! belong to that category. feeds.category stores the category title,
//! matching the Python schema.

use crate::db::Db;
use opml::{OPML, Outline};

fn collect(
    outlines: &[Outline],
    category: Option<&str>,
    out: &mut Vec<(String, String, Option<String>)>,
) {
    for o in outlines {
        if let Some(url) = &o.xml_url {
            let title = if o.text.trim().is_empty() {
                url.clone()
            } else {
                o.text.clone()
            };
            out.push((url.clone(), title, category.map(str::to_string)));
        } else if !o.outlines.is_empty() {
            collect(&o.outlines, Some(o.text.as_str()), out);
        }
    }
}

/// Import subscriptions from OPML text. Feeds whose URL is already
/// subscribed are skipped. Returns how many feeds were added.
pub fn import_opml(db: &Db, xml: &str) -> Result<usize, String> {
    let doc = OPML::from_str(xml).map_err(|e| format!("OPML parse failed: {e}"))?;
    let mut found = Vec::new();
    collect(&doc.body.outlines, None, &mut found);
    let mut added = 0;
    for (url, title, category) in found {
        if db.feed_url_exists(&url).map_err(|e| format!("{e}"))? {
            continue;
        }
        db.add_feed(&url, &title, category.as_deref())
            .map_err(|e| format!("add failed: {e}"))?;
        added += 1;
    }
    Ok(added)
}

/// Export all subscriptions as OPML 2.0, feeds grouped by category.
pub fn export_opml(db: &Db) -> Result<String, String> {
    let feeds = db.feeds().map_err(|e| format!("{e}"))?;
    let mut doc = OPML {
        head: Some(opml::Head {
            title: Some("BlindRSS Subscriptions".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut uncategorized: Vec<Outline> = Vec::new();
    let mut by_category: std::collections::BTreeMap<String, Vec<Outline>> =
        std::collections::BTreeMap::new();
    for f in &feeds {
        let outline = Outline {
            text: f.title.clone(),
            xml_url: Some(f.url.clone()),
            r#type: Some("rss".to_string()),
            ..Default::default()
        };
        match &f.category {
            Some(cat) if !cat.trim().is_empty() => {
                by_category.entry(cat.clone()).or_default().push(outline);
            }
            _ => uncategorized.push(outline),
        }
    }
    for (cat, outlines) in by_category {
        doc.body.outlines.push(Outline {
            text: cat,
            outlines,
            ..Default::default()
        });
    }
    doc.body.outlines.extend(uncategorized);
    doc.to_string()
        .map_err(|e| format!("OPML write failed: {e}"))
}
