//! Article translation (Python `core/translation.py`).
//!
//! Six LLM providers behind one call: grok (xAI), openai,
//! groq and qwen speak the OpenAI chat-completions shape;
//! openrouter adds its ranking headers; gemini uses
//! generateContent. Text is chunked at ~3,500 chars on
//! paragraph boundaries, capped at 50,000 total, temperature
//! 0, with Python's exact system prompt. A configured model
//! wins; otherwise the provider's default candidate list is
//! tried in order until one answers.

/// Provider keys in UI order.
pub const PROVIDERS: [&str; 6] = ["grok", "openai", "groq", "gemini", "openrouter", "qwen"];

pub const PROVIDER_LABELS: [&str; 6] = [
    "Grok (xAI)",
    "OpenAI",
    "Groq",
    "Gemini",
    "OpenRouter",
    "Qwen",
];

const SYSTEM_PROMPT: &str = "You are a translation engine. Translate the user's text into the requested target language. Preserve line breaks, headings, and overall formatting. Return only the translated text with no commentary.";
const CHUNK_CHARS: usize = 3500;
const MAX_TOTAL_CHARS: usize = 50_000;

fn endpoint(provider: &str) -> Option<&'static str> {
    match provider {
        "grok" => Some("https://api.x.ai/v1/chat/completions"),
        "openai" => Some("https://api.openai.com/v1/chat/completions"),
        "groq" => Some("https://api.groq.com/openai/v1/chat/completions"),
        "openrouter" => Some("https://openrouter.ai/api/v1/chat/completions"),
        "qwen" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions"),
        _ => None,
    }
}

fn default_models(provider: &str) -> &'static [&'static str] {
    match provider {
        "grok" => &[
            "grok-4-1-fast-non-reasoning",
            "grok-4-fast-non-reasoning",
            "grok-3-mini",
            "grok-4-1-fast-reasoning",
            "grok-4-fast-reasoning",
        ],
        "openai" => &["gpt-5-mini", "gpt-4.1-mini", "gpt-4o-mini", "gpt-4.1-nano"],
        "groq" => &[
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
            "mixtral-8x7b-32768",
        ],
        "openrouter" => &["openrouter/free", "openrouter/auto"],
        "gemini" => &[
            "gemini-3-flash-preview",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
            "gemini-2.0-flash-lite",
        ],
        "qwen" => &["qwen-mt-plus", "qwen-mt-turbo", "qwen-plus", "qwen-turbo"],
        _ => &[],
    }
}

/// Split text into ~CHUNK_CHARS chunks on paragraph (then
/// line, then hard) boundaries.
pub fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        if para.chars().count() > CHUNK_CHARS {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut rest = para.to_string();
            while rest.chars().count() > CHUNK_CHARS {
                let cut = rest
                    .char_indices()
                    .nth(CHUNK_CHARS)
                    .map(|(i, _)| i)
                    .unwrap_or(rest.len());
                chunks.push(rest[..cut].to_string());
                rest = rest[cut..].to_string();
            }
            current = rest;
            continue;
        }
        let candidate = if current.is_empty() {
            para.to_string()
        } else {
            format!("{current}\n\n{para}")
        };
        if candidate.chars().count() > CHUNK_CHARS && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current = para.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn chat_once(
    provider: &str,
    api_key: &str,
    model: &str,
    target_language: &str,
    chunk: &str,
) -> Result<String, String> {
    let user_prompt = format!("Target language: {target_language}\n\nText to translate:\n{chunk}");
    if provider == "gemini" {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        );
        let body = serde_json::json!({
            "systemInstruction": {"parts": [{"text": SYSTEM_PROMPT}]},
            "contents": [{"role": "user", "parts": [{"text": user_prompt}]}],
            "generationConfig": {"temperature": 0},
        });
        let mut resp = crate::http::agent()
            .post(&url)
            .header("x-goog-api-key", api_key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| format!("gemini request: {e}"))?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("gemini read: {e}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("gemini response: {e}"))?;
        return value
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "gemini returned no text".to_string());
    }
    let url = endpoint(provider).ok_or_else(|| format!("unknown provider {provider}"))?;
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt},
        ],
        "temperature": 0,
        "stream": false,
    });
    let req = crate::http::agent()
        .post(url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json");
    let req = if provider == "openrouter" {
        req.header("HTTP-Referer", "https://github.com/serrebidev/BlindRSS")
            .header("X-Title", "BlindRSS")
    } else {
        req
    };
    let mut resp = req
        .send(body.to_string())
        .map_err(|e| format!("{provider} request: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{provider} read: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{provider} response: {e}"))?;
    value
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{provider} returned no text"))
}

/// Translate `text` into `target_language` with the
/// configured provider. Config keys: translation_provider,
/// translation_<provider>_api_key, translation_<provider>_model.
pub fn translate_text(
    cfg: &crate::config::Config,
    text: &str,
    target_language: &str,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }
    let provider = cfg.get_str("translation_provider", "grok").to_string();
    if !PROVIDERS.contains(&provider.as_str()) {
        return Err(format!("unknown translation provider: {provider}"));
    }
    let api_key = cfg
        .get_str(&format!("translation_{provider}_api_key"), "")
        .to_string();
    if api_key.trim().is_empty() {
        return Err(format!(
            "no API key configured for the {provider} translation provider"
        ));
    }
    let configured_model = cfg
        .get_str(&format!("translation_{provider}_model"), "")
        .trim()
        .to_string();
    let models: Vec<String> = if configured_model.is_empty() {
        default_models(&provider)
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![configured_model]
    };
    let truncated: String = text.chars().take(MAX_TOTAL_CHARS).collect();
    let mut out: Vec<String> = Vec::new();
    for chunk in chunk_text(&truncated) {
        let mut last_err = String::new();
        let mut done: Option<String> = None;
        for model in &models {
            match chat_once(&provider, &api_key, model, target_language, &chunk) {
                Ok(t) => {
                    done = Some(t);
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        out.push(done.ok_or(last_err)?);
    }
    Ok(out.join("\n\n"))
}

/// A tiny stopword-based source-language guess (article_lang:
/// used to skip translating text already in the target
/// language). Returns an ISO code or "" when unsure.
pub fn detect_language(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .take(400)
        .collect();
    if words.len() < 8 {
        return "";
    }
    let score = |stops: &[&str]| words.iter().filter(|w| stops.contains(w)).count();
    let candidates: [(&str, &[&str]); 7] = [
        (
            "en",
            &[
                "the", "and", "of", "to", "in", "is", "that", "for", "with", "on",
            ],
        ),
        (
            "es",
            &[
                "el", "la", "de", "que", "en", "los", "del", "las", "una", "por",
            ],
        ),
        (
            "fr",
            &[
                "le", "la", "de", "des", "les", "et", "est", "dans", "que", "pour",
            ],
        ),
        (
            "de",
            &[
                "der", "die", "und", "ist", "das", "den", "von", "mit", "sich", "des",
            ],
        ),
        (
            "pt",
            &["o", "a", "de", "e", "do", "da", "em", "um", "para", "com"],
        ),
        (
            "it",
            &[
                "il", "di", "che", "e", "la", "per", "un", "una", "del", "sono",
            ],
        ),
        (
            "nl",
            &[
                "de", "het", "een", "van", "ik", "te", "dat", "die", "in", "een",
            ],
        ),
    ];
    let (best, best_score) = candidates
        .iter()
        .map(|(code, stops)| (*code, score(stops)))
        .max_by_key(|(_, s)| *s)
        .unwrap_or(("", 0));
    if best_score >= 3 { best } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_and_detection() {
        let text = (0..200)
            .map(|i| format!("Paragraph {i} with some words in it."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|c| c.chars().count() <= CHUNK_CHARS + 100)
        );
        assert_eq!(
            detect_language("the quick brown fox and the dog are in the garden with that ball"),
            "en"
        );
        assert_eq!(
            detect_language("el zorro marrón rápido y el perro están en el jardín con la pelota"),
            "es"
        );
        assert_eq!(detect_language("short"), "");
    }
}
