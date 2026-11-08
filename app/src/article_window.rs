//! Standalone reader with cached plain/rich modes and window-local commands.

use patois::t;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wxdragon::event::webview_events::WebViewEvents;
use wxdragon::prelude::*;
use wxdragon::widgets::webview::{WebView, WebViewBackend};

pub fn open(title: &str, body: &str, url: Option<&str>) {
    open_impl(title, body, url, None, false, false, "");
}

/// Open for a stored article whose author is known (the main
/// window's Open Article / deleted-article paths): the author
/// feeds the full-text render header, as Python's
/// `render_full_article(fallback_author=...)` does.
pub fn open_article(title: &str, body: &str, url: Option<&str>, author: &str) {
    open_impl(title, body, url, None, false, false, author);
}

/// Open Article from URL (Python `gui/article_window.py`,
/// Audit 8 G13): the window opens immediately in the chosen
/// starting mode and the page loads asynchronously under the
/// window's generation guard; plain results pass through
/// automatic translation, and a failed load shows Python's
/// readable failure text in the reader instead of leaving it
/// on "Loading article...".
pub fn open_url(url: &str, rich: bool) {
    open_impl(
        url,
        "Loading article...",
        Some(url),
        Some(rich),
        true,
        true,
        "",
    );
}

/// Python `core/open_article.py` `_reason_from`: one
/// readable line out of a failure, whitespace-collapsed and
/// capped.
fn failure_reason(reason: &str) -> String {
    let collapsed = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 400 {
        format!(
            "{}...",
            collapsed.chars().take(397).collect::<String>().trim_end()
        )
    } else {
        collapsed
    }
}

/// Python `failure_text`: the plain stand-in shown when the
/// page could not be read.
fn failure_text(url: &str, reason: &str) -> String {
    let reason = failure_reason(reason);
    let mut parts = vec!["This page could not be read.".to_string()];
    if !reason.is_empty() {
        parts.push(reason);
    }
    if !url.is_empty() {
        parts.push(format!("Address: {url}"));
    }
    parts.push(
        "Try opening it in your browser, or import that site's cookies from Tools > Import Site Cookies if it is behind a check."
            .to_string(),
    );
    format!("{}\n", parts.join("\n\n"))
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Python `failure_html`: the same explanation as a fragment
/// for the rich reader, a real `<article>` with an `<h1>` so
/// heading navigation still lands somewhere.
fn failure_html(url: &str, reason: &str) -> String {
    let reason = failure_reason(reason);
    let mut body = "<h1>This page could not be read.</h1>".to_string();
    if !reason.is_empty() {
        body.push_str(&format!("<p>{}</p>", html_escape(&reason)));
    }
    if !url.is_empty() {
        body.push_str(&format!(
            "<p><a href=\"{}\">{}</a></p>",
            html_escape(url),
            html_escape(url)
        ));
    }
    body.push_str(
        "<p>Try opening it in your browser, or import that site's cookies from Tools &gt; Import Site Cookies if it is behind a check.</p>",
    );
    format!("<article>{body}</article>")
}

/// Python `title_from_rendered_html`: the `<h1>` the rich
/// renderer wrote, for the window's caption.
fn title_from_rich_html(fragment: &str) -> String {
    let lower = fragment.to_lowercase();
    let Some(start) = lower.find("<h1") else {
        return String::new();
    };
    let Some(open_end) = lower[start..].find('>') else {
        return String::new();
    };
    let inner_start = start + open_end + 1;
    let Some(close) = lower[inner_start..].find("</h1>") else {
        return String::new();
    };
    let inner = &fragment[inner_start..inner_start + close];
    // Strip any nested tags, then collapse whitespace.
    let mut text = String::new();
    let mut in_tag = false;
    for ch in inner.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Python `utils.collapse_blank_lines`: drop empty lines,
/// one paragraph per line.
fn collapse_blank_lines(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn open_impl(
    title: &str,
    body: &str,
    url: Option<&str>,
    start_rich: Option<bool>,
    start_load: bool,
    translate_plain: bool,
    author: &str,
) {
    let frame = Frame::builder()
        .with_title(&format!("BlindRSS - {title}"))
        .with_size(Size::new(760, 560))
        .build();
    let layout = BoxSizer::builder(Orientation::Vertical).build();
    let rich = CheckBox::builder(&frame)
        .with_label("Show in &HTML view")
        .build();
    layout.add(&rich, 0, SizerFlag::All, 4);
    let reader = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::Rich2)
        .build();
    crate::msaa_names::set_name(&reader, &t("Article text"));
    reader.set_value(body);
    layout.add(&reader, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let web = WebView::is_backend_available(WebViewBackend::Default)
        .then(|| WebView::builder(&frame).build());
    let loading = Rc::new(Cell::new(false));
    if let Some(view) = web {
        layout.add(&view, 1, SizerFlag::Expand | SizerFlag::All, 4);
        view.show(false);
        let flag = loading.clone();
        view.on_loaded(move |_| flag.set(false));
        let flag = loading.clone();
        view.on_navigating(move |event| {
            if flag.get() || !event.get_target().unwrap_or_default().is_empty() {
                return;
            }
            if let Some(url) = event.get_url()
                && !crate::rich_embed_url(&url)
                && let Some(url) = crate::rich_external_url(&url)
            {
                event.event.event.veto();
                crate::open_article_link(&frame, &url);
            }
        });
        view.on_new_window(move |event| {
            if let Some(url) = event.get_url().and_then(|u| crate::rich_external_url(&u)) {
                crate::open_article_link(&frame, &url);
            }
        });
    }
    frame.set_sizer(layout, true);
    frame.create_status_bar(1, 0, -1, "Article status");
    let url = url.unwrap_or("").to_string();
    rich.enable(web.is_some() && !url.is_empty());
    // Menus match Python's article_window.py (Batch 41a /
    // Audit 6 G1.5): File / Edit / View, same items, labels
    // and order. Ids keep the existing handler mapping below.
    let file_menu = Menu::builder()
        .append_item(
            4002,
            "Open in &Browser",
            "Open this address in your web browser",
        )
        .append_item(4001, "Re&load", "Fetch this page again")
        .append_separator()
        .append_item(4006, "Close &Window\tCtrl+W", "Close this article window")
        .build();
    let edit_menu = Menu::builder()
        .append_item(
            4004,
            "&Copy Text",
            "Copy the whole article to the clipboard",
        )
        .append_item(
            4003,
            "Copy &Link",
            "Copy this article's address to the clipboard",
        )
        .build();
    let view_menu = Menu::builder()
        .append_check_item(
            4005,
            "&HTML View\tCtrl+Shift+H",
            "Show the page as formatted HTML instead of plain text",
        )
        .build();
    frame.set_menu_bar(
        MenuBar::builder()
            .append(file_menu, "&File")
            .append(edit_menu, "&Edit")
            .append(view_menu, "&View")
            .build(),
    );
    let cached = Rc::new(RefCell::new(None::<String>));
    // The whole current plain-mode result (Audit 8 G12):
    // Copy Text copies this, not whatever the plain control
    // happens to hold while richer content is on screen.
    let current_text = Rc::new(RefCell::new(body.to_string()));
    let generation = Rc::new(Cell::new(0_u64));
    let (tx, rx) = std::sync::mpsc::channel::<(u64, bool, Result<(String, String), String>)>();
    let load: Rc<dyn Fn(bool)> = {
        let url = url.clone();
        let title = title.to_string();
        let fallback = body.to_string();
        let cached = cached.clone();
        let generation = generation.clone();
        let loading = loading.clone();
        Rc::new(move |reload| {
            let want_rich = rich.is_checked() && web.is_some();
            if let Some(bar) = frame.get_menu_bar() {
                bar.check_item(4005, want_rich);
            }
            if !want_rich {
                reader.show(true);
                if let Some(view) = web {
                    view.show(false);
                }
                frame.layout();
            }
            if !reload {
                if !want_rich {
                    return;
                }
                if let Some(html) = cached.borrow().as_ref() {
                    if let Some(view) = web {
                        loading.set(true);
                        view.set_page(&crate::rich_document(html), &url);
                        reader.show(false);
                        view.show(true);
                        frame.layout();
                    }
                    return;
                }
            } else {
                *cached.borrow_mut() = None;
            }
            if url.is_empty() {
                return;
            }
            generation.set(generation.get() + 1);
            let token = generation.get();
            frame.set_status_text("Loading article...", 0);
            let tx = tx.clone();
            let url = url.clone();
            let title = title.clone();
            let fallback = fallback.clone();
            std::thread::spawn(move || {
                let jar = blindrss_core::cookies::load_jar_beside(&crate::config_path());
                let header = blindrss_core::cookies::cookie_header_for(&jar, &url);
                let result = if want_rich {
                    blindrss_core::article_html::render_rich_article(
                        &url,
                        &fallback,
                        &title,
                        "",
                        "",
                        &header,
                        "",
                        "",
                        &crate::ui_language(),
                    )
                    .map(|html| (String::new(), html))
                    .ok_or_else(|| "Could not load HTML; switch to the text reader.".into())
                } else {
                    match blindrss_core::extract::fetch_full_text(
                        &url,
                        &header,
                        Some(&fallback),
                        "",
                    )
                    .0
                    {
                        // The extracted title rides along so
                        // the caption follows the page, and
                        // URL windows translate the plain
                        // result (Python's ArticleWindow
                        // translate callback, Audit 8 G13).
                        blindrss_core::ExtractOutcome::Article {
                            title: page_title,
                            text,
                            ..
                        } => {
                            let text = if translate_plain {
                                crate::auto_translate_text(text)
                            } else {
                                text
                            };
                            Ok((page_title, text))
                        }
                        blindrss_core::ExtractOutcome::Gate { reason } => Err(reason),
                        blindrss_core::ExtractOutcome::Empty => Err("No article text found".into()),
                    }
                };
                let _ = tx.send((token, want_rich, result));
            });
        })
    };
    let action = load.clone();
    rich.on_toggled(move |_| action(false));
    let action = load.clone();
    let source = url.clone();
    let cached_menu = cached.clone();
    let current_text_menu = current_text.clone();
    frame.on_menu(move |event| match event.get_id() {
        4001 => action(true),
        4002 => {
            if !source.is_empty() {
                crate::open_article_link(&frame, &source);
            }
        }
        4003 => {
            Clipboard::get().set_text(&source);
        }
        4004 => {
            // Copy Text copies the WHOLE current result in
            // the active mode (Python `on_copy_text`, Audit 8
            // G12): in HTML view that is the text of the
            // loaded document — never the hidden plain
            // control's stale contents. Ctrl+C in the reader
            // keeps its selection semantics untouched.
            let text = if rich.is_checked() && web.is_some() {
                match cached_menu.borrow().as_ref() {
                    Some(html) => collapse_blank_lines(&blindrss_core::extract::html_to_text(html)),
                    None => current_text_menu.borrow().clone(),
                }
            } else {
                current_text_menu.borrow().clone()
            };
            if !text.trim().is_empty() {
                Clipboard::get().set_text(&text);
            }
        }
        4005 => {
            rich.set_value(!rich.is_checked() && web.is_some() && !source.is_empty());
            action(false);
        }
        4006 => {
            frame.close(false);
        }
        _ => event.skip(true),
    });
    let timer = Box::leak(Box::new(Timer::new(&frame)));
    let source = url;
    let current_text_tick = current_text.clone();
    let title_tick = title.to_string();
    let author_tick = author.to_string();
    timer.on_tick(move |_| {
        while let Ok((token, html, result)) = rx.try_recv() {
            if token != generation.get() || html != rich.is_checked() {
                continue;
            }
            match result {
                Ok((_title, text)) if html => {
                    *cached.borrow_mut() = Some(text.clone());
                    let page_title = title_from_rich_html(&text);
                    if !page_title.is_empty() {
                        frame.set_title(&format!("BlindRSS - {page_title}"));
                    }
                    if let Some(view) = web {
                        loading.set(true);
                        view.set_page(&crate::rich_document(&text), &source);
                        reader.show(false);
                        view.show(true);
                        frame.layout();
                    }
                    frame.set_status_text("HTML article loaded", 0);
                }
                Ok((page_title, text)) => {
                    *current_text_tick.borrow_mut() = text.clone();
                    // The plain result is a full-text render:
                    // it carries Python's Title:/Author: header
                    // (render_full_article), not the bare body.
                    let display_title = if page_title.is_empty() {
                        title_tick.as_str()
                    } else {
                        page_title.as_str()
                    };
                    reader.set_value(&crate::fulltext_reader_text(
                        display_title,
                        &author_tick,
                        &text,
                    ));
                    if !page_title.is_empty() {
                        frame.set_title(&format!("BlindRSS - {page_title}"));
                    }
                    frame.set_status_text("Article loaded", 0);
                }
                Err(error) => {
                    // URL windows show Python's readable
                    // failure content in the reader itself
                    // (Audit 8 G13); article windows keep the
                    // status-only behaviour.
                    if start_load && html {
                        if let Some(view) = web {
                            loading.set(true);
                            view.set_page(
                                &crate::rich_document(&failure_html(&source, &error)),
                                &source,
                            );
                            reader.show(false);
                            view.show(true);
                            frame.layout();
                        }
                        frame.set_status_text("This page could not be read.", 0);
                    } else if start_load {
                        let text = failure_text(&source, &error);
                        *current_text_tick.borrow_mut() = text.clone();
                        reader.set_value(&text);
                        frame.set_status_text("This page could not be read.", 0);
                    } else {
                        frame.set_status_text(&error, 0);
                    }
                }
            }
        }
    });
    timer.start(150, false);
    frame.on_close(move |event| {
        timer.stop();
        event.skip(true);
    });
    frame.bind_internal(EventType::CHAR_HOOK, move |event| {
        let key = event.get_key_code().unwrap_or(0);
        if key == 27 {
            frame.close(false);
            event.skip(false);
        } else if key == 13
            && reader.has_focus()
            && !event.control_down()
            && !event.alt_down()
            && blindrss_core::config::Config::load(&crate::config_path())
                .unwrap_or_default()
                .get_bool("article_structure_links", false)
            && crate::open_reader_link(&reader, &frame)
        {
            event.skip(false);
        } else {
            event.skip(true);
        }
    });
    frame.show(true);
    reader.set_focus();
    if start_load {
        // URL window (Audit 8 G13): start in the mode the
        // Open Article dialog chose and fetch at once.
        rich.set_value(start_rich.unwrap_or(false) && web.is_some());
        load(true);
    } else {
        let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
        if cfg.get_bool("full_text_rich_view", false) && web.is_some() {
            rich.set_value(true);
            load(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_text_source_is_the_whole_current_result() {
        // Audit 8 G12: Copy Text in rich mode copies the
        // loaded document's text with blank lines collapsed
        // (Python `collapse_blank_lines(html_to_text(...))`),
        // so what is copied matches what is on screen.
        let html = "<h1>Head</h1><p>One</p>\n\n<p>Two</p>";
        let text = collapse_blank_lines(&blindrss_core::extract::html_to_text(html));
        assert!(text.contains("One"));
        assert!(text.contains("Two"));
        assert!(!text.contains("\n\n"));
        assert_eq!(collapse_blank_lines("a\r\n\r\n  \nb  \n"), "a\nb");
    }

    #[test]
    fn failure_content_matches_python_open_article() {
        // Audit 8 G13: the plain failure stand-in carries the
        // headline, the collapsed reason, the address and the
        // recovery advice, newline-terminated; the HTML one
        // is a real article with an h1 and an escaped link.
        let text = failure_text("https://example.com/x", "HTTP  404\n gone");
        assert!(text.starts_with("This page could not be read.\n\nHTTP 404 gone\n\n"));
        assert!(text.contains("Address: https://example.com/x"));
        assert!(text.ends_with("behind a check.\n"));
        let html = failure_html("https://example.com/?a=1&b=2", "no <tags>");
        assert!(html.starts_with("<article><h1>This page could not be read.</h1>"));
        assert!(html.contains("no &lt;tags&gt;"));
        assert!(html.contains("href=\"https://example.com/?a=1&amp;b=2\""));
    }

    #[test]
    fn rich_title_comes_from_the_h1() {
        assert_eq!(
            title_from_rich_html(
                "<article><h1 class=\"x\">Hello <b>World</b></h1><p>x</p></article>"
            ),
            "Hello World"
        );
        assert_eq!(title_from_rich_html("<p>none</p>"), "");
    }
}
