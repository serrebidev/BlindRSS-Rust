//! Audit 8 Task 12: automatic reader/translation lifecycle —
//! the core half. Python `core/translation.py` is the spec:
//! S20 (ordered fallback vectors: three Qwen regions, the full
//! Grok candidate list, Groq-key routing), S21 (array chat
//! content, joined Gemini parts, blank results rejected so the
//! original text survives), and the O07/O08 cache-variant
//! identity (`::tr[provider:lang:model]`) plus the delayed-result
//! guard (a result applies only to the article, generation and
//! variant it was requested for).

use blindrss_core::config::Config;
use blindrss_core::reader_cache::{ReaderCache, TranslationRequest};
use blindrss_core::translate::{
    default_models, effective_provider, endpoints, extract_chat_text, extract_gemini_text,
    looks_like_groq_key, settings_from_config, translate_text_with_endpoints,
};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

// ---------- fallback vectors (S20) ----------

#[test]
fn model_vectors_match_python() {
    assert_eq!(
        default_models("grok"),
        [
            "grok-4-1-fast-non-reasoning",
            "grok-4-fast-non-reasoning",
            "grok-3-mini",
            "grok-4-1-fast-reasoning",
            "grok-4-fast-reasoning",
            "grok-4-0709",
            "grok-3-fast",
            "grok-3",
            "grok-4",
            "grok-beta",
        ]
    );
    assert_eq!(
        default_models("qwen"),
        [
            "qwen-mt-plus",
            "qwen-mt-flash",
            "qwen-mt-lite",
            "qwen-plus",
            "qwen-plus-latest",
            "qwen3.5-plus",
            "qwen-turbo",
        ]
    );
    assert_eq!(
        default_models("groq"),
        [
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
            "mixtral-8x7b-32768",
        ]
    );
    assert_eq!(
        default_models("openai"),
        ["gpt-5-mini", "gpt-4.1-mini", "gpt-4o-mini", "gpt-4.1-nano"]
    );
    assert_eq!(
        default_models("gemini"),
        [
            "gemini-3-flash-preview",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
            "gemini-2.0-flash-lite",
        ]
    );
    assert_eq!(
        default_models("openrouter"),
        ["openrouter/free", "openrouter/auto"]
    );
}

#[test]
fn qwen_endpoints_match_python_regions_in_order() {
    assert_eq!(
        endpoints("qwen"),
        vec![
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            "https://dashscope-us.aliyuncs.com/compatible-mode/v1/chat/completions",
        ]
    );
    // Every other chat provider has exactly one endpoint.
    for p in ["grok", "openai", "groq", "openrouter"] {
        assert_eq!(endpoints(p).len(), 1, "{p}");
    }
}

#[test]
fn groq_key_routing_matches_python() {
    // Python `_looks_like_groq_key`: a gsk_ key entered for Grok
    // is routed to Groq (users confuse the two providers).
    assert!(looks_like_groq_key("gsk_abc123"));
    assert!(looks_like_groq_key("  GSK_ABC123 "));
    assert!(!looks_like_groq_key("xai-abc123"));
    assert!(!looks_like_groq_key(""));
    assert_eq!(effective_provider("grok", "gsk_abc123"), "groq");
    assert_eq!(effective_provider("grok", "xai-abc123"), "grok");
    assert_eq!(effective_provider("groq", "gsk_abc123"), "groq");
    assert_eq!(effective_provider("openai", "gsk_abc123"), "openai");
}

// ---------- response shapes (S21) ----------

#[test]
fn chat_content_array_parts_are_joined() {
    // Python `_extract_chat_completion_text`: content may be a
    // list of parts; their text values join.
    let payload = serde_json::json!({
        "choices": [{"message": {"content": [
            {"type": "text", "text": "Hola "},
            {"type": "text", "text": "mundo"},
            {"type": "image", "url": "https://example.com/x.png"},
        ]}}]
    });
    assert_eq!(extract_chat_text(&payload), "Hola mundo");
    // Plain string content is stripped.
    let payload = serde_json::json!({
        "choices": [{"message": {"content": "  Bonjour  "}}]
    });
    assert_eq!(extract_chat_text(&payload), "Bonjour");
    // No choices / no content reads as empty (a failure upstream,
    // never a replacement text).
    assert_eq!(extract_chat_text(&serde_json::json!({})), "");
    assert_eq!(
        extract_chat_text(&serde_json::json!({"choices": [{"message": {"content": "   "}}]})),
        ""
    );
}

#[test]
fn gemini_parts_are_all_joined() {
    // Python `_extract_gemini_completion_text` joins EVERY part,
    // not just the first.
    let payload = serde_json::json!({
        "candidates": [{"content": {"parts": [
            {"text": "Bonjour "},
            {"text": "le monde"},
        ]}}]
    });
    assert_eq!(extract_gemini_text(&payload), "Bonjour le monde");
    let payload = serde_json::json!({
        "candidates": [{"content": {"parts": [{"text": "  "}]}}]
    });
    assert_eq!(extract_gemini_text(&payload), "");
    assert_eq!(extract_gemini_text(&serde_json::json!({})), "");
}

// ---------- settings + variant identity (O07/O08) ----------

fn translation_cfg(provider: &str, key: &str) -> Config {
    let mut cfg = Config::default();
    cfg.set_path("translation_enabled", serde_json::json!(true));
    cfg.set_path("translation_provider", serde_json::json!(provider));
    cfg.set_path(
        &format!("translation_{provider}_api_key"),
        serde_json::json!(key),
    );
    cfg
}

#[test]
fn settings_require_enabled_and_a_key() {
    // Python `_translation_runtime_config`: no config dict at
    // all unless translation is enabled AND the provider's key
    // is present — the automatic flows key off exactly this.
    let cfg = translation_cfg("grok", "xai-test");
    let settings = settings_from_config(&cfg).expect("settings");
    assert_eq!(settings.provider, "grok");
    assert_eq!(settings.target_language, "en");
    assert_eq!(settings.model, "");
    assert_eq!(settings.timeout_s, 45);
    assert_eq!(settings.chunk_chars, 3500);
    // Variant identity (Python `_translation_fulltext_cache_suffix`).
    assert_eq!(settings.cache_suffix(), "::tr[grok:en]");

    let mut cfg = translation_cfg("grok", "xai-test");
    cfg.set_path("translation_grok_model", serde_json::json!("Grok-4"));
    cfg.set_path("translation_target_language", serde_json::json!("EN"));
    let settings = settings_from_config(&cfg).expect("settings");
    assert_eq!(settings.cache_suffix(), "::tr[grok:en:grok-4]");

    // Disabled -> no settings, however complete the rest is.
    let mut cfg = translation_cfg("grok", "xai-test");
    cfg.set_path("translation_enabled", serde_json::json!(false));
    assert!(settings_from_config(&cfg).is_none());
    // Enabled but keyless -> no settings.
    let mut cfg = Config::default();
    cfg.set_path("translation_enabled", serde_json::json!(true));
    assert!(settings_from_config(&cfg).is_none());
    // Unknown provider falls back to grok (Python clamps it).
    let mut cfg = translation_cfg("grok", "xai-test");
    cfg.set_path("translation_provider", serde_json::json!("bogus"));
    let settings = settings_from_config(&cfg).expect("settings");
    assert_eq!(settings.provider, "grok");
    // Timeout/chunk clamps are Python's.
    let mut cfg = translation_cfg("grok", "xai-test");
    cfg.set_path("translation_timeout_seconds", serde_json::json!(9999));
    cfg.set_path("translation_chunk_chars", serde_json::json!(10));
    let settings = settings_from_config(&cfg).expect("settings");
    assert_eq!(settings.timeout_s, 180);
    assert_eq!(settings.chunk_chars, 500);
}

// ---------- delayed-result guard (O08) ----------

#[test]
fn translation_request_applies_only_to_its_identity() {
    // The delayed A-after-B case: a translation requested for
    // article A must never land after the user moved to B, the
    // generation moved on, or the provider/language/model
    // variant changed underneath it.
    let req = TranslationRequest {
        article_id: "a".into(),
        generation: 7,
        variant: "::tr[grok:en]".into(),
    };
    assert!(req.is_current(7, "a", "::tr[grok:en]"));
    assert!(!req.is_current(8, "a", "::tr[grok:en]")); // selection moved
    assert!(!req.is_current(7, "b", "::tr[grok:en]")); // other article
    assert!(!req.is_current(7, "a", "::tr[groq:en]")); // provider changed
    assert!(!req.is_current(7, "a", "::tr[grok:fr]")); // language changed
    assert!(!req.is_current(7, "a", "")); // translation turned off
}

// ---------- cache invalidation for alt changes (G16) ----------

#[test]
fn reader_cache_prefix_invalidation_is_article_scoped() {
    let mut cache = ReaderCache::default();
    cache.insert("art-1:aaaa".into(), "one".into(), "web", 0);
    cache.insert("art-2:bbbb".into(), "two".into(), "web", 0);
    cache.remove_prefix("art-1:");
    assert!(cache.get("art-1:aaaa", false).is_none());
    assert_eq!(cache.get("art-2:bbbb", false).unwrap().0, "two");
}

// ---------- loopback provider servers ----------

struct TestServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

fn serve(responses: Vec<(u16, String)>) -> TestServer {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let queue: Arc<Mutex<VecDeque<(u16, String)>>> =
        Arc::new(Mutex::new(responses.into_iter().collect()));
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            let body_at = loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break None,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf);
                        if let Some(pos) = text.find("\r\n\r\n") {
                            let len = text[..pos]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if buf.len() >= pos + 4 + len {
                                break Some(pos + 4);
                            }
                        }
                    }
                    Err(_) => break None,
                }
            };
            let Some(body_at) = body_at else { continue };
            let text = String::from_utf8_lossy(&buf).to_string();
            let request_line = text.lines().next().unwrap_or("").to_string();
            let body = text[body_at..].to_string();
            recorded
                .lock()
                .unwrap()
                .push(format!("{request_line}\n{body}"));
            let (status, payload) = queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or((500, "{}".to_string()));
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                404 => "Not Found",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    TestServer {
        url: format!("http://{addr}"),
        requests,
    }
}

fn chat_ok(text: &str) -> (u16, String) {
    (
        200,
        serde_json::json!({"choices": [{"message": {"content": text}}]}).to_string(),
    )
}

#[test]
fn chat_array_content_translates_over_loopback() {
    let server = serve(vec![(
        200,
        serde_json::json!({"choices": [{"message": {"content": [
            {"type": "text", "text": "Hola "},
            {"type": "text", "text": "mundo"},
        ]}}]})
        .to_string(),
    )]);
    let cfg = translation_cfg("grok", "xai-test");
    let url = format!("{}/v1/chat/completions", server.url);
    let out = translate_text_with_endpoints(&cfg, "Hello world.", "es", &[&url], None)
        .expect("translation");
    assert_eq!(out, "Hola mundo");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn blank_chat_result_is_an_error_not_a_replacement() {
    // Python raises "returned an empty translation response";
    // the caller keeps the original text. A whitespace success
    // must never become the article.
    let server = serve(vec![chat_ok("   ")]);
    let cfg = translation_cfg("grok", "xai-test");
    let url = format!("{}/v1/chat/completions", server.url);
    let err = translate_text_with_endpoints(&cfg, "Hello world.", "es", &[&url], None)
        .expect_err("blank must fail");
    assert!(err.contains("empty translation"), "{err}");
}

#[test]
fn model_fallback_advances_only_on_model_errors() {
    // First candidate answers 404 with a model-not-found body:
    // Python `_retryable_model_error` advances to the next
    // candidate, whose answer is used.
    let server = serve(vec![
        (
            404,
            serde_json::json!({"error": {"message": "model 'grok-4-1-fast-non-reasoning' not found"}})
                .to_string(),
        ),
        chat_ok("Bonjour"),
    ]);
    let cfg = translation_cfg("grok", "xai-test");
    let url = format!("{}/v1/chat/completions", server.url);
    let out = translate_text_with_endpoints(&cfg, "Hello.", "fr", &[&url], None)
        .expect("fallback translation");
    assert_eq!(out, "Bonjour");
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].contains("grok-4-fast-non-reasoning"),
        "{}",
        requests[1]
    );
}

#[test]
fn auth_error_aborts_without_burning_the_model_list() {
    // A 401 is not a model error: Python raises immediately
    // instead of trying every candidate against a dead key.
    let server = serve(vec![(
        401,
        serde_json::json!({"error": {"message": "invalid api key"}}).to_string(),
    )]);
    let cfg = translation_cfg("grok", "xai-bad");
    let url = format!("{}/v1/chat/completions", server.url);
    let err = translate_text_with_endpoints(&cfg, "Hello.", "fr", &[&url], None)
        .expect_err("401 must fail");
    assert!(err.contains("401"), "{err}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn groq_key_under_grok_uses_groq_models() {
    // The routed call speaks to Groq, so its default candidate
    // list is Groq's — the request must name a Groq model even
    // though the configured provider is grok.
    let server = serve(vec![chat_ok("Bonjour")]);
    let cfg = translation_cfg("grok", "gsk_testkey");
    let url = format!("{}/v1/chat/completions", server.url);
    let out = translate_text_with_endpoints(&cfg, "Hello.", "fr", &[&url], None)
        .expect("routed translation");
    assert_eq!(out, "Bonjour");
    let requests = server.requests.lock().unwrap();
    assert!(
        requests[0].contains("llama-3.3-70b-versatile"),
        "{}",
        requests[0]
    );
}

#[test]
fn qwen_falls_back_across_region_endpoints() {
    // First region refuses the connection; the second answers.
    // Python `_translate_chunk_qwen` walks the endpoint list in
    // order per chunk.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let server = serve(vec![chat_ok("Bonjour")]);
    let cfg = translation_cfg("qwen", "sk-test");
    let dead_url = format!("http://{dead_addr}/compatible-mode/v1/chat/completions");
    let url = format!("{}/compatible-mode/v1/chat/completions", server.url);
    let out = translate_text_with_endpoints(&cfg, "Hello.", "fr", &[&dead_url, &url], None)
        .expect("second region answers");
    assert_eq!(out, "Bonjour");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn gemini_multipart_translates_and_blank_fails() {
    let template = |base: &str| format!("{base}/v1beta/models/{{model}}:generateContent");
    let server = serve(vec![(
        200,
        serde_json::json!({"candidates": [{"content": {"parts": [
            {"text": "Bonjour "},
            {"text": "le monde"},
        ]}}]})
        .to_string(),
    )]);
    let cfg = translation_cfg("gemini", "gm-test");
    let out = translate_text_with_endpoints(
        &cfg,
        "Hello world.",
        "fr",
        &[],
        Some(&template(&server.url)),
    )
    .expect("gemini translation");
    assert_eq!(out, "Bonjour le monde");
    // The request went to the first candidate model's URL.
    let requests = server.requests.lock().unwrap();
    assert!(
        requests[0].contains("gemini-3-flash-preview:generateContent"),
        "{}",
        requests[0]
    );
    drop(requests);

    let server = serve(vec![(
        200,
        serde_json::json!({"candidates": [{"content": {"parts": [{"text": "  "}]}}]}).to_string(),
    )]);
    let err = translate_text_with_endpoints(
        &cfg,
        "Hello world.",
        "fr",
        &[],
        Some(&template(&server.url)),
    )
    .expect_err("blank gemini must fail");
    assert!(err.contains("empty translation"), "{err}");
}
