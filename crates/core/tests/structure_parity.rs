use blindrss_core::extract::{StructureOptions, html_to_text_with_structure};

#[test]
fn inline_links_use_the_caret_then_the_first_link_on_its_line() {
    use blindrss_core::article_html::inline_link_at;
    let line = "See first (https://example.com/one), then https://example.com/two.";
    assert_eq!(
        inline_link_at(line, 55).as_deref(),
        Some("https://example.com/two")
    );
    assert_eq!(
        inline_link_at(line, 0).as_deref(),
        Some("https://example.com/one")
    );
    assert_eq!(inline_link_at("No link here", 4), None);
    assert_eq!(inline_link_at("javascript:bad()", 4), None);
    assert_eq!(
        inline_link_at(
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            0
        )
        .as_deref(),
        Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
    );
}

#[test]
fn structure_markers_preserve_nested_lists_quotes_and_links() {
    let options = StructureOptions {
        tables: true,
        headings: true,
        lists: true,
        quotes: true,
        links: true,
    };
    let html = "<h2>Why</h2><ol><li>First<ul><li>Nested</li></ul></li><li>Second</li></ol><blockquote><p>A quote</p></blockquote><p>See <a href='https://example.com/news'>report</a> and <a href='https://example.com/raw'>https://example.com/raw</a>.</p>";
    let text = html_to_text_with_structure(html, false, options);
    for marker in [
        "Heading level 2: Why",
        "1. First",
        "• Nested",
        "2. Second",
        "Quote:",
        "End of quote.",
        "report (https://example.com/news)",
    ] {
        assert!(text.contains(marker), "{marker}: {text}");
    }
    assert!(!text.contains("https://example.com/raw (https://example.com/raw)"));
}

#[test]
fn data_table_associates_headers_but_layout_table_stays_prose() {
    let html = "<table><caption>Results</caption><tr><th>Year</th><th>Patched</th></tr><tr><td>2025</td><td>140</td></tr></table><table><tr><td><p>Article prose</p></td><td>More prose</td></tr></table>";
    let text = html_to_text_with_structure(html, false, StructureOptions::default());
    assert!(
        text.contains("Table with 1 rows and 2 columns: Results."),
        "{text}"
    );
    assert!(text.contains("Row 1: Year: 2025; Patched: 140."), "{text}");
    assert!(text.contains("End of table."));
    assert!(text.contains("Article prose"));
    assert_eq!(text.matches("Table with").count(), 1);
    let plain = html_to_text_with_structure(
        html,
        false,
        StructureOptions {
            tables: false,
            ..Default::default()
        },
    );
    assert!(!plain.contains("Table with"));
}

#[test]
fn disabled_extra_markers_and_unsafe_link_targets_stay_plain() {
    let html = "<h3>Heading</h3><ul><li>Item</li></ul><blockquote>Quote</blockquote><a href='javascript:bad()'>bad</a><a href='https://example.com/two words'>space</a>";
    let text = html_to_text_with_structure(html, false, StructureOptions::default());
    assert!(!text.contains("Heading level"));
    assert!(!text.contains('•'));
    assert!(!text.contains("End of quote"));
    let links = html_to_text_with_structure(
        html,
        false,
        StructureOptions {
            links: true,
            ..Default::default()
        },
    );
    assert!(!links.contains("javascript"));
    assert!(!links.contains("two words"));
}

#[test]
fn configured_structure_reaches_full_text_extraction() {
    let mut config = blindrss_core::config::Config::default();
    for key in ["headings", "lists", "quotes", "links"] {
        config.set_path(&format!("article_structure_{key}"), serde_json::json!(true));
    }
    blindrss_core::extract::configure_text_structure(&config);
    let prose = "The research team carefully checked every observation before publishing these results. Their report describes the methods and evidence in sufficient detail for independent review. ".repeat(8);
    let html = format!(
        "<html><head><title>Research</title></head><body><article><h2>Findings</h2><p>{prose}</p><ul><li>A useful detail for readers.</li></ul><p>{prose}</p></article></body></html>"
    );
    let outcome =
        blindrss_core::extract::extract_article(&html, "https://example.com/research", None);
    blindrss_core::extract::configure_text_structure(&blindrss_core::config::Config::default());
    let blindrss_core::extract::ExtractOutcome::Article { text, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(text.contains("Heading level 2: Findings"), "{text}");
    assert!(text.contains("• A useful detail"), "{text}");
}

#[test]
fn semantic_source_documents_keep_replies_and_links_in_both_readers() {
    let html = "<html><head><title>Topic</title></head><body><h1>Topic</h1><section class='blindrss-groupsio-post'><h2>#1 Message by Alice</h2><p>First message.</p></section><section><h2>#2 Message by Bob</h2><p>Reply with <a href='https://example.com/reference'>reference</a>.</p><script>unsafe_script()</script><a href='javascript:unsafe()'>Unsafe target</a></section></body></html>";
    let rich = blindrss_core::article_html::clean_article_html(
        html,
        "https://groups.io/g/readers/topic/123",
        false,
    );
    for content in [
        "Message by Alice",
        "Message by Bob",
        "First message.",
        "https://example.com/reference",
    ] {
        assert!(rich.contains(content), "missing {content}: {rich}");
    }
    assert!(!rich.contains("unsafe_script"));
    assert!(!rich.contains("javascript:"));
    let plain = html_to_text_with_structure(
        &rich,
        false,
        StructureOptions {
            headings: true,
            links: true,
            ..Default::default()
        },
    );
    assert!(
        plain.contains("Heading level 2: #1 Message by Alice"),
        "{plain}"
    );
    assert!(
        plain.contains("Heading level 2: #2 Message by Bob"),
        "{plain}"
    );
    assert!(
        plain.contains("reference (https://example.com/reference)"),
        "{plain}"
    );
}
