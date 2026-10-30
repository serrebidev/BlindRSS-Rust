//! i18n catalog, shortcuts registry, and help parsing tests.

use blindrss_core::help::{to_plain_text, topics};
use blindrss_core::i18n::Catalog;
use blindrss_core::shortcuts::{commands, conflicts, effective, normalize_accelerator};
use std::collections::HashMap;

#[test]
fn po_parse_multiline_and_escapes() {
    let po = r#"msgid ""
msgstr "Content-Type: text/plain; charset=UTF-8\n"

msgid "Refresh All Feeds"
msgstr "Alle Feeds aktualisieren"

msgid "Multi "
"line key"
msgstr "Mehrzeilig \"zitiert\""
"#;
    let cat = Catalog::parse(po);
    assert_eq!(cat.get("Refresh All Feeds"), "Alle Feeds aktualisieren");
    assert_eq!(cat.get("Multi line key"), "Mehrzeilig \"zitiert\"");
    assert_eq!(cat.get("Untranslated"), "Untranslated");
}

#[test]
fn shortcuts_registry() {
    let cmds = commands();
    assert!(
        cmds.len() >= 60,
        "expected the full Python table, got {}",
        cmds.len()
    );
    assert_eq!(normalize_accelerator("control+shift+r"), "Ctrl+Shift+R");
    assert_eq!(normalize_accelerator("f5"), "F5");
    assert_eq!(normalize_accelerator(""), "");
    let mut overrides = HashMap::new();
    overrides.insert("feeds.add".to_string(), "ctrl+alt+n".to_string());
    let eff = effective(&overrides);
    assert_eq!(eff.get("feeds.add").map(|s| s.as_str()), Some("Ctrl+Alt+N"));
    assert_eq!(eff.get("feeds.refresh_all").map(|s| s.as_str()), Some("F5"));
    let mut dup = HashMap::new();
    dup.insert("feeds.add".to_string(), "F5".to_string());
    let eff2 = effective(&dup);
    let conf = conflicts(&eff2);
    assert!(conf.contains_key("F5"));
}

#[test]
fn real_python_assets_parse() {
    // Against the Python checkout when present (skips elsewhere):
    // the shipped German catalog and English guide must parse.
    let root = std::path::Path::new("/home/hatch/workspace/blindrss-work");
    let po = root.join("locale/de/LC_MESSAGES/blindrss.po");
    if po.exists() {
        let text = std::fs::read_to_string(&po).unwrap();
        let cat = Catalog::parse(&text);
        assert!(cat.len() > 100, "de catalog too small: {}", cat.len());
    }
    let guide_path = root.join("docs/help/en.md");
    if guide_path.exists() {
        let guide = std::fs::read_to_string(&guide_path).unwrap();
        let ts = topics(&guide);
        assert!(ts.len() > 20, "en guide topics too few: {}", ts.len());
    }
}

#[test]
fn help_topics_split() {
    let guide = "# BlindRSS Help\n\nWelcome text.\n\n## Getting Started\n\nAdd a feed with **Ctrl+N**. See [the site](https://example.com).\n\n## Playback\n\nPress play.\n";
    let ts = topics(guide);
    assert_eq!(ts.len(), 3);
    assert_eq!(ts[1].title, "Getting Started");
    let plain = to_plain_text(&ts[1].body);
    assert!(plain.contains("Add a feed with Ctrl+N. See the site."));
    assert!(!plain.contains("**"));
}
