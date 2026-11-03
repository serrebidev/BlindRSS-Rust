//! OPML import/export (Stage 2, slice 3).
//!
//! Category nesting follows the common convention Python also reads:
//! an outline without xmlUrl is a category; feed outlines inside it
//! belong to that category. feeds.category stores the category title,
//! matching the Python schema.

use crate::db::{Db, Feed, make_category_path};
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
            // Nested category outlines build the full path
            // ("News / Tech"), matching Python's category identity.
            let child = make_category_path(category, o.text.trim());
            collect(&o.outlines, Some(&child), out);
        }
    }
}

/// Import subscriptions from OPML text. Feeds whose URL is already
/// subscribed are skipped. Returns how many feeds were added.
pub fn import_opml(db: &Db, xml: &str) -> Result<usize, String> {
    import_opml_under(db, xml, None)
}

/// Import OPML under a target category ("Import OPML Here"):
/// every imported feed's category is prefixed with the target
/// path, and the category rows are created to match.
pub fn import_opml_under(db: &Db, xml: &str, target: Option<&str>) -> Result<usize, String> {
    let doc = OPML::from_str(xml).map_err(|e| format!("OPML parse failed: {e}"))?;
    let mut found = Vec::new();
    collect(&doc.body.outlines, None, &mut found);
    let mut added = 0;
    for (url, title, category) in found {
        if db.feed_url_exists(&url).map_err(|e| format!("{e}"))? {
            continue;
        }
        let category = match (target, category) {
            (Some(t), Some(c)) => Some(make_category_path(Some(t), &c)),
            (Some(t), None) => Some(t.to_string()),
            (None, c) => c,
        };
        if let Some(cat) = &category {
            let _ = db.ensure_category_path(cat);
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
