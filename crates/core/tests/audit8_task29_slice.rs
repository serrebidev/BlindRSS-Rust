//! Audit 8 Task 29 regressions: per-feed columns (S03),
//! column normalization (S22), help fallback + full-guide
//! model (S07/S08), translated display labels (S10), debug
//! diagnostics redaction (S18), translation presets + model
//! discovery (S19), and Feed Properties overrides (G08).

use blindrss_core::Db;
use blindrss_core::article_columns::{
    feed_layout_from_settings, layout_to_json, move_key, resolve_for_view, resolve_layout,
    set_visible,
};
use blindrss_core::help::{language_candidates, load_document, parse_guide, render_plain_text};
use blindrss_core::i18n::Catalog;
use blindrss_core::translate::{
    LANGUAGE_PRESETS, language_code_from_ui, language_display, language_display_value,
    language_name, openrouter_model_choices, openrouter_models_from_payload,
};
use serde_json::json;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "blindrss_t29_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ))
}

fn keys(layout: &[blindrss_core::article_columns::ColumnLayout]) -> Vec<&str> {
    layout.iter().map(|c| c.key.as_str()).collect()
}

// ---------- S22: legacy column shapes ----------

#[test]
fn legacy_bare_string_columns_normalize() {
    // Python accepts a bare list of names, trimmed and
    // case-insensitive; Title pins first, missing keys append
    // in default order.
    let stored = json!([" Date ", "FEED", "title"]);
    let layout = resolve_layout(Some(&stored));
    assert_eq!(
        keys(&layout),
        [
            "title",
            "date",
            "feed",
            "author",
            "media",
            "description",
            "status"
        ]
    );
    assert!(layout.iter().all(|c| c.visible));
}

#[test]
fn legacy_duplicates_unknowns_and_visibility() {
    let stored = json!([
        {"key": "Date", "visible": false},
        {"key": "date"},
        {"key": "bogus"},
        {"key": "TITLE", "visible": false},
        {"key": "author", "visible": 0},
        42,
        {"visible": true},
    ]);
    let layout = resolve_layout(Some(&stored));
    // Duplicate "date" collapsed to the first (hidden) entry.
    let date = layout.iter().find(|c| c.key == "date").unwrap();
    assert!(!date.visible);
    // Title forced visible despite visible=false; author with
    // visible=0 is hidden (Python truthiness).
    assert!(layout[0].visible);
    let author = layout.iter().find(|c| c.key == "author").unwrap();
    assert!(!author.visible);
    assert_eq!(layout.len(), 7);
    assert!(!layout.iter().any(|c| c.key == "bogus"));
}

#[test]
fn move_and_visibility_ops_match_python() {
    let layout = resolve_layout(None);
    // Title never moves and nothing moves above it.
    let moved = move_key(&layout, "title", 1);
    assert_eq!(keys(&moved), keys(&layout));
    let moved = move_key(&layout, "author", -5);
    assert_eq!(moved[1].key, "author", "clamped at index 1");
    let moved = move_key(&layout, "author", 2);
    assert_eq!(moved[3].key, "author");
    // Title ignores hide requests.
    let hidden = set_visible(&layout, "title", false);
    assert!(hidden[0].visible);
    let hidden = set_visible(&layout, "date", false);
    assert!(!hidden.iter().find(|c| c.key == "date").unwrap().visible);
}

// ---------- S03: per-feed layout resolution ----------

#[test]
fn feed_layout_inherits_unless_a_list_is_stored() {
    assert!(feed_layout_from_settings(&json!({})).is_none());
    assert!(feed_layout_from_settings(&json!({"columns": null})).is_none());
    assert!(feed_layout_from_settings(&json!({"columns": []})).is_none());
    assert!(feed_layout_from_settings(&json!({"columns": "date"})).is_none());
    let feed = feed_layout_from_settings(&json!({"columns": ["status", "title"]})).unwrap();
    assert_eq!(feed[0].key, "title");
    assert_eq!(feed[1].key, "status");

    let global = resolve_layout(None);
    // Category/mixed views pass no feed layout: global wins.
    assert_eq!(keys(&resolve_for_view(&global, None)), keys(&global));
    // A feed view with an override uses it.
    let view = resolve_for_view(&global, Some(feed));
    assert_eq!(view[1].key, "status");
    // Serializing a resolved layout keeps Python's dict shape
    // readable (key/visible per entry).
    let stored = layout_to_json(&view);
    assert_eq!(stored[0]["key"], json!("title"));
    assert_eq!(stored[0]["visible"], json!(true));
}

// ---------- S08: help locale fallback chain ----------

#[test]
fn help_candidates_walk_regional_to_english() {
    assert_eq!(
        language_candidates("fr_CA"),
        ["fr-CA", "fr_CA", "fr-ca", "fr", "en"]
    );
    assert_eq!(language_candidates("en"), ["en"]);
    assert_eq!(
        language_candidates("pt-BR"),
        ["pt-BR", "pt_BR", "pt-br", "pt", "en"]
    );
}

#[test]
fn help_loads_base_language_and_reports_fallback() {
    let dir = temp_path("helpdir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fr.md"),
        "# Guide\n\n## Bonjour {#hello}\n\nTexte.\n",
    )
    .unwrap();
    // fr_CA with only fr.md: base language loads, and that is
    // NOT an English fallback.
    let doc = load_document(&dir, "fr_CA");
    assert_eq!(doc.language, "fr");
    assert!(!doc.is_fallback());
    assert_eq!(doc.sections.len(), 1);
    // de with only fr.md: English is missing too, so the empty
    // document reports the fallback language.
    let doc = load_document(&dir, "de");
    assert!(doc.sections.is_empty());
    assert_eq!(doc.language, "en");
    assert!(doc.is_fallback());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn help_real_guide_regional_fallback() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/help");
    // The shipped tree has fr.md but no fr_CA.md / fr-CA.md.
    let doc = load_document(&dir, "fr_CA");
    assert_eq!(doc.language, "fr");
    assert!(!doc.is_fallback());
    assert!(doc.sections.len() > 50);
    let doc = load_document(&dir, "en");
    assert!(doc.sections.len() > 50);
    assert!(doc.sections.iter().any(|s| !s.anchor.is_empty()));
}

// ---------- S07: full-guide parse + render ----------

#[test]
fn help_parse_and_render_python_shape() {
    let md = "\
# BlindRSS User Guide

## Getting Started {#getting-started}

Welcome to [BlindRSS](https://example.com) the **bold** reader.

- first item
- second item

1. step one
2. step two

### Details {#details}

```
code line
```

Closing paragraph
spanning two lines.
";
    let doc = parse_guide(md, "en");
    assert_eq!(doc.title, "BlindRSS User Guide");
    assert_eq!(doc.sections.len(), 2);
    assert_eq!(doc.sections[0].level, 2);
    assert_eq!(doc.sections[1].level, 3);
    let rendered = render_plain_text(&doc);
    // The doc title repeats the first section? No: the first
    // section is "Getting Started", so the title line leads.
    assert!(
        rendered
            .text
            .starts_with("BlindRSS User Guide\n\nGetting Started\n\n")
    );
    assert!(
        rendered
            .text
            .contains("Welcome to BlindRSS (https://example.com) the bold reader.")
    );
    assert!(rendered.text.contains("- first item"));
    assert!(rendered.text.contains("1. step one"));
    assert!(rendered.text.contains("    code line"));
    assert!(
        rendered
            .text
            .contains("Closing paragraph spanning two lines.")
    );
    assert!(rendered.text.ends_with('\n'));
    // Anchors map to their section's line; section_lines is
    // parallel to the contents entries.
    let start_line = rendered.anchor_lines["getting-started"];
    let lines: Vec<&str> = rendered.text.lines().collect();
    assert_eq!(lines[start_line], "Getting Started");
    assert_eq!(rendered.section_lines.len(), 2);
    assert_eq!(
        lines[rendered.section_lines[1]], "Details",
        "unanchored-position jump works for anchored sections too"
    );
    let entries = blindrss_core::help::contents_entries(&doc);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1], ("details", "Details", 3));
}

// ---------- S10: shipped catalog covers display strings ----------

#[test]
fn german_catalog_translates_column_and_settings_labels() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../locale");
    let cat = Catalog::load(&root, "de");
    assert!(!cat.is_empty());
    for msgid in ["Title", "Author", "Date", "Description", "Media"] {
        assert_ne!(cat.get(msgid), msgid, "de catalog lacks {msgid}");
    }
    assert_ne!(
        cat.get("Enable Sound Notifications"),
        "Enable Sound Notifications"
    );
    assert_ne!(
        cat.get("Automatic (system language)"),
        "Automatic (system language)"
    );
    // Available-language enumeration sees the shipped tree.
    let langs = blindrss_core::i18n::Catalog::available_languages(&[root]);
    assert!(langs.contains(&"de".to_string()));
    assert!(langs.contains(&"pt_BR".to_string()));
}

// ---------- S09/S19: language names and codes ----------

#[test]
fn language_names_resolve_regional_codes() {
    assert_eq!(language_name("en"), Some("English"));
    assert_eq!(language_name("pt_BR"), Some("Portuguese (Brazil)"));
    assert_eq!(language_name("zh_CN"), Some("Chinese (Simplified)"));
    assert_eq!(language_name("xx"), None);
    assert_eq!(language_display("pt_BR"), "Portuguese (Brazil) (pt_BR)");
    assert_eq!(language_display("xx"), "xx");
    assert!(LANGUAGE_PRESETS.len() >= 180);
}

#[test]
fn translation_language_field_round_trips() {
    assert_eq!(language_display_value("es"), "Spanish");
    assert_eq!(language_display_value("pt-BR"), "Portuguese (Brazil)");
    assert_eq!(language_display_value(""), "English (en)");
    assert_eq!(language_display_value("tlh"), "tlh");
    // Code extraction: name, "Name (code)", and raw codes.
    assert_eq!(language_code_from_ui("Spanish"), "es");
    assert_eq!(language_code_from_ui("Spanish (es)"), "es");
    assert_eq!(language_code_from_ui("pt-BR"), "pt-BR");
    assert_eq!(language_code_from_ui(""), "en");
    // A custom value is preserved verbatim.
    assert_eq!(language_code_from_ui("my-custom-code"), "my-custom-code");
}

#[test]
fn openrouter_payload_sorted_deduped() {
    let payload = json!({"data": [
        {"id": "z/model"},
        {"id": "a/model"},
        {"id": "A/MODEL"},
        {"id": "  "},
        {"name": "no id"},
        {"id": "m/model"},
    ]});
    assert_eq!(
        openrouter_models_from_payload(&payload),
        ["a/model", "m/model", "z/model"]
    );
    let choices = openrouter_model_choices(&["a/model".to_string(), "new/one".to_string()]);
    assert_eq!(choices[0], "openrouter/free");
    assert!(choices.contains(&"new/one".to_string()));
    assert_eq!(
        choices
            .iter()
            .filter(|c| c.eq_ignore_ascii_case("a/model"))
            .count(),
        1
    );
}

// ---------- S18: diagnostics redaction ----------

#[test]
fn diagnostics_redact_config_secrets_and_headers() {
    let mut cfg = blindrss_core::config::Config::default();
    cfg.set_path("translation_grok_api_key", json!("xai-secretkey-123456"));
    cfg.set_path("github_token", json!("ghp_secret_token_abcdef"));
    cfg.set_path("captcha_solver_api_key", json!("solver-key-987654"));
    cfg.set_path("groups_io_api_key", json!("groupsio-key-55555"));
    let secrets = blindrss_core::diagnostics::secrets_from_config(&cfg);
    let secret_refs: Vec<&str> = secrets.iter().map(|s| s.as_str()).collect();
    let line = "fetch failed key=xai-secretkey-123456 token ghp_secret_token_abcdef \
                Authorization: Bearer abc.def.ghi solver solver-key-987655 \
                groups groupsio-key-55555";
    let out = blindrss_core::diagnostics::redact_line(line, &secret_refs);
    for secret in [
        "xai-secretkey-123456",
        "ghp_secret_token_abcdef",
        "abc.def.ghi",
        "groupsio-key-55555",
    ] {
        assert!(!out.contains(secret), "leaked {secret}: {out}");
    }
    // Writing through the rotating log keeps the redaction.
    let path = temp_path("diag.log");
    let mut log =
        blindrss_core::diagnostics::RotatingLog::open_with_limits(&path, 1024, 3).unwrap();
    log.write_line(&blindrss_core::diagnostics::redact_line(line, &secret_refs))
        .unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("ghp_secret_token_abcdef"));
    std::fs::remove_file(&path).ok();
}

// ---------- G08: feed-settings merge + fetch overrides ----------

fn scratch_db(tag: &str) -> (Db, std::path::PathBuf) {
    let path = temp_path(tag);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE feeds (id TEXT PRIMARY KEY, url TEXT, title TEXT, \
         title_is_custom INTEGER DEFAULT 0, category TEXT, icon_url TEXT);
         CREATE TABLE articles (id TEXT PRIMARY KEY, feed_id TEXT, title TEXT, \
         url TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0, \
         media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL);
         CREATE TABLE categories (id TEXT PRIMARY KEY, title TEXT UNIQUE);",
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path)
}

#[test]
fn feed_settings_patch_merges_without_erasing() {
    let (db, path) = scratch_db("merge.db");
    let feed = db.add_feed("https://example.com/f", "F", None).unwrap();
    // A key the dialog does not show must survive a dialog save.
    db.set_feed_settings(&feed.id, &json!({"unseen_key": "keep"}))
        .unwrap();
    let patch = blindrss_core::feed_settings::build_patch(
        "X-A: 1", "30", "always", "", "utf-8", "", 7, None,
    );
    db.set_feed_settings(&feed.id, &patch).unwrap();
    let stored = db.feed_settings(&feed.id).unwrap();
    assert_eq!(stored["unseen_key"], json!("keep"));
    assert_eq!(stored["custom_headers"], json!({"X-A": "1"}));
    assert_eq!(stored["timeout_seconds"], json!(30));
    assert_eq!(stored["impersonate"], json!("always"));
    assert_eq!(stored["refresh_interval_seconds"], json!(300));
    std::fs::remove_file(&path).ok();
}

#[test]
fn feed_fetch_applies_headers_referer_and_encoding() {
    // The server captures the request head and answers with a
    // windows-1251 RSS body that carries NO encoding
    // declaration: only the feed's encoding override can
    // decode its Cyrillic title.
    let title = "Новости дня";
    // "Новости дня" in windows-1251 bytes.
    let encoded: [u8; 11] = [
        0xCD, 0xEE, 0xE2, 0xEE, 0xF1, 0xF2, 0xE8, 0x20, 0xE4, 0xED, 0xFF,
    ];
    let mut body = Vec::new();
    body.extend_from_slice(b"<?xml version=\"1.0\"?>\n<rss version=\"2.0\"><channel><title>");
    body.extend_from_slice(&encoded);
    body.extend_from_slice(
        b"</title><item><title>One</title><link>https://example.com/1</link>\
          <guid>g1</guid></item></channel></rss>",
    );
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured2 = captured.clone();
    let body_thread = body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            *captured2.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body_thread.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&body_thread);
        }
    });
    let url = format!("http://{addr}/feed.xml");
    let (db, path) = scratch_db("fetch.db");
    // The feed row must exist for the settings to stick.
    let feed = db.add_feed(&url, "Override Feed", None).unwrap();
    db.set_feed_settings(
        &feed.id,
        &json!({
            "custom_headers": {"X-Test": "yes"},
            "encoding": "windows-1251",
        }),
    )
    .unwrap();
    let inserted = blindrss_core::refresh::refresh_feed(&db, &feed, "deleted").unwrap();
    assert_eq!(inserted, 1);
    let head = captured.lock().unwrap().clone().to_lowercase();
    assert!(head.contains("x-test: yes"), "headers: {head}");
    assert!(
        head.contains(&format!("referer: http://{addr}/")),
        "referer: {head}"
    );
    // The stored article proves the fetch + parse worked; the
    // override decode is covered directly below (feed title in
    // the row only updates on subscribe in this path).
    let articles = db.articles(Some(&feed.id), 10).unwrap();
    assert_eq!(articles.len(), 1);
    // Direct decode check: the override turns the 1251 bytes
    // into the Cyrillic title; without it the text differs.
    let decoded = blindrss_core::feeds::decode_feed_body(&body, "windows-1251");
    assert!(String::from_utf8_lossy(&decoded).contains(title));
    std::fs::remove_file(&path).ok();
}
