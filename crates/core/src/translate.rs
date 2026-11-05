//! Article translation (Python `core/translation.py`).
//!
//! Six LLM providers behind one call: grok (xAI), openai,
//! groq and qwen speak the OpenAI chat-completions shape;
//! openrouter adds its ranking headers; gemini uses
//! generateContent. Text is chunked at ~3,500 chars on
//! newline/space boundaries (Python `_iter_text_chunks`),
//! capped at 50,000 total, temperature 0, with Python's exact
//! system prompt.
//!
//! Fallback semantics are Python's (Audit 8 S20/S21): the
//! default model candidate lists are tried in order, but a
//! candidate is skipped only for a model-unavailability error
//! (`_retryable_model_error`) — an auth failure or a blank
//! response fails the chunk at once, so a translation failure
//! can never masquerade as a (blank) translation. Qwen walks
//! its three regional endpoints in order; a Groq-shaped key
//! (`gsk_…`) configured for Grok is routed to Groq. Chat
//! content may be a string or an array of text parts; Gemini
//! parts all join. Chunk results are stripped by the response
//! parsers and joined with no separator, as in Python.

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
const GEMINI_URL_TEMPLATE: &str =
    "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent";

/// Chat-completions endpoints per provider, in the order
/// Python tries them (Qwen: international, China mainland,
/// United States — `_QWEN_CHAT_COMPLETIONS_ENDPOINTS`).
/// Gemini is not a chat provider and returns an empty vec.
pub fn endpoints(provider: &str) -> Vec<&'static str> {
    match provider {
        "grok" => vec!["https://api.x.ai/v1/chat/completions"],
        "openai" => vec!["https://api.openai.com/v1/chat/completions"],
        "groq" => vec!["https://api.groq.com/openai/v1/chat/completions"],
        "openrouter" => vec!["https://openrouter.ai/api/v1/chat/completions"],
        "qwen" => vec![
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            "https://dashscope-us.aliyuncs.com/compatible-mode/v1/chat/completions",
        ],
        _ => vec![],
    }
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        "grok" => "Grok",
        "openai" => "OpenAI",
        "groq" => "Groq",
        "openrouter" => "OpenRouter",
        "gemini" => "Gemini",
        "qwen" => "Qwen",
        _ => "Translation",
    }
}

/// The provider's default model candidates, in Python's
/// order (`_DEFAULT_*_MODEL_CANDIDATES`). A configured model
/// replaces the whole list with that single candidate.
pub fn default_models(provider: &str) -> &'static [&'static str] {
    match provider {
        "grok" => &[
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
        "qwen" => &[
            "qwen-mt-plus",
            "qwen-mt-flash",
            "qwen-mt-lite",
            "qwen-plus",
            "qwen-plus-latest",
            "qwen3.5-plus",
            "qwen-turbo",
        ],
        _ => &[],
    }
}

/// Readable language names for translation targets and the
/// interface-language picker (Python
/// `_TRANSLATION_LANGUAGE_PRESETS` in gui/dialogs.py): the
/// English name a screen reader shows, mapped to the code
/// that is actually stored. Names are display strings — the
/// app translates them at display time; codes never change.
pub const LANGUAGE_PRESETS: &[(&str, &str)] = &[
    ("Abkhazian", "ab"),
    ("Afar", "aa"),
    ("Afrikaans", "af"),
    ("Akan", "ak"),
    ("Albanian", "sq"),
    ("Amharic", "am"),
    ("Arabic", "ar"),
    ("Aragonese", "an"),
    ("Armenian", "hy"),
    ("Assamese", "as"),
    ("Avaric", "av"),
    ("Avestan", "ae"),
    ("Aymara", "ay"),
    ("Azerbaijani", "az"),
    ("Bambara", "bm"),
    ("Bashkir", "ba"),
    ("Basque", "eu"),
    ("Belarusian", "be"),
    ("Bengali", "bn"),
    ("Bislama", "bi"),
    ("Bosnian", "bs"),
    ("Breton", "br"),
    ("Bulgarian", "bg"),
    ("Burmese", "my"),
    ("Catalan", "ca"),
    ("Chamorro", "ch"),
    ("Chechen", "ce"),
    ("Chichewa", "ny"),
    ("Chinese (Simplified)", "zh-CN"),
    ("Chinese (Traditional)", "zh-TW"),
    ("Chinese", "zh"),
    ("Church Slavic", "cu"),
    ("Chuvash", "cv"),
    ("Cornish", "kw"),
    ("Corsican", "co"),
    ("Cree", "cr"),
    ("Croatian", "hr"),
    ("Czech", "cs"),
    ("Danish", "da"),
    ("Divehi", "dv"),
    ("Dutch (Belgium)", "nl-BE"),
    ("Dutch", "nl"),
    ("Dzongkha", "dz"),
    ("English", "en"),
    ("Esperanto", "eo"),
    ("Estonian", "et"),
    ("Ewe", "ee"),
    ("Faroese", "fo"),
    ("Fijian", "fj"),
    ("Finnish", "fi"),
    ("French", "fr"),
    ("Fulah", "ff"),
    ("Galician", "gl"),
    ("Ganda", "lg"),
    ("Georgian", "ka"),
    ("German", "de"),
    ("Guarani", "gn"),
    ("Gujarati", "gu"),
    ("Haitian", "ht"),
    ("Hausa", "ha"),
    ("Hebrew", "he"),
    ("Herero", "hz"),
    ("Hindi", "hi"),
    ("Hiri Motu", "ho"),
    ("Hungarian", "hu"),
    ("Icelandic", "is"),
    ("Ido", "io"),
    ("Igbo", "ig"),
    ("Indonesian", "id"),
    (
        "Interlingua (International Auxiliary Language Association)",
        "ia",
    ),
    ("Interlingue", "ie"),
    ("Inuktitut", "iu"),
    ("Inupiaq", "ik"),
    ("Irish", "ga"),
    ("Italian", "it"),
    ("Japanese", "ja"),
    ("Javanese", "jv"),
    ("Kalaallisut", "kl"),
    ("Kannada", "kn"),
    ("Kanuri", "kr"),
    ("Kashmiri", "ks"),
    ("Kazakh", "kk"),
    ("Khmer", "km"),
    ("Kikuyu", "ki"),
    ("Kinyarwanda", "rw"),
    ("Kirghiz", "ky"),
    ("Komi", "kv"),
    ("Kongo", "kg"),
    ("Korean", "ko"),
    ("Kuanyama", "kj"),
    ("Kurdish", "ku"),
    ("Lao", "lo"),
    ("Latin", "la"),
    ("Latvian", "lv"),
    ("Limburgan", "li"),
    ("Lingala", "ln"),
    ("Lithuanian", "lt"),
    ("Luba-Katanga", "lu"),
    ("Luxembourgish", "lb"),
    ("Macedonian", "mk"),
    ("Malagasy", "mg"),
    ("Malay (macrolanguage)", "ms"),
    ("Malayalam", "ml"),
    ("Maltese", "mt"),
    ("Manx", "gv"),
    ("Maori", "mi"),
    ("Marathi", "mr"),
    ("Marshallese", "mh"),
    ("Modern Greek (1453-)", "el"),
    ("Mongolian", "mn"),
    ("Nauruan", "na"),
    ("Navajo", "nv"),
    ("Ndonga", "ng"),
    ("Nepali (macrolanguage)", "ne"),
    ("North Ndebele", "nd"),
    ("Northern Sami", "se"),
    ("Norwegian", "no"),
    ("Norwegian Bokmal", "nb"),
    ("Norwegian Nynorsk", "nn"),
    ("Occitan (post 1500)", "oc"),
    ("Ojibwa", "oj"),
    ("Oriya (macrolanguage)", "or"),
    ("Oromo", "om"),
    ("Ossetian", "os"),
    ("Pali", "pi"),
    ("Panjabi", "pa"),
    ("Persian", "fa"),
    ("Polish", "pl"),
    ("Portuguese (Brazil)", "pt-BR"),
    ("Portuguese (Portugal)", "pt-PT"),
    ("Portuguese", "pt"),
    ("Pushto", "ps"),
    ("Quechua", "qu"),
    ("Romanian", "ro"),
    ("Romansh", "rm"),
    ("Rundi", "rn"),
    ("Russian", "ru"),
    ("Samoan", "sm"),
    ("Sango", "sg"),
    ("Sanskrit", "sa"),
    ("Sardinian", "sc"),
    ("Scottish Gaelic", "gd"),
    ("Serbian", "sr"),
    ("Serbo-Croatian", "sh"),
    ("Shona", "sn"),
    ("Sichuan Yi", "ii"),
    ("Sindhi", "sd"),
    ("Sinhala", "si"),
    ("Slovak", "sk"),
    ("Slovenian", "sl"),
    ("Somali", "so"),
    ("South Ndebele", "nr"),
    ("Southern Sotho", "st"),
    ("Spanish", "es"),
    ("Sundanese", "su"),
    ("Swahili (macrolanguage)", "sw"),
    ("Swati", "ss"),
    ("Swedish", "sv"),
    ("Tagalog", "tl"),
    ("Tahitian", "ty"),
    ("Tajik", "tg"),
    ("Tamil", "ta"),
    ("Tatar", "tt"),
    ("Telugu", "te"),
    ("Thai", "th"),
    ("Tibetan", "bo"),
    ("Tigrinya", "ti"),
    ("Tonga (Tonga Islands)", "to"),
    ("Tsonga", "ts"),
    ("Tswana", "tn"),
    ("Turkish", "tr"),
    ("Turkmen", "tk"),
    ("Twi", "tw"),
    ("Uighur", "ug"),
    ("Ukrainian", "uk"),
    ("Urdu", "ur"),
    ("Uzbek", "uz"),
    ("Venda", "ve"),
    ("Vietnamese", "vi"),
    ("Volapuk", "vo"),
    ("Walloon", "wa"),
    ("Welsh", "cy"),
    ("Western Frisian", "fy"),
    ("Wolof", "wo"),
    ("Xhosa", "xh"),
    ("Yiddish", "yi"),
    ("Yoruba", "yo"),
    ("Zhuang", "za"),
    ("Zulu", "zu"),
];

/// The preset name for a language code, trying the code
/// as-is, then with `_` replaced by `-`, then its base
/// language (Python's Settings picker name resolution:
/// locale directories use underscores like `pt_BR`, the
/// presets use BCP-47 hyphens like `pt-BR`, and a regional
/// code with no preset of its own falls back to the base).
pub fn language_name(code: &str) -> Option<&'static str> {
    let by_code = |wanted: &str| {
        LANGUAGE_PRESETS
            .iter()
            .find(|(_, c)| *c == wanted)
            .map(|(name, _)| *name)
    };
    by_code(code)
        .or_else(|| by_code(&code.replace('_', "-")))
        .or_else(|| {
            let base = code.split(['_', '-']).next().unwrap_or("");
            by_code(base)
        })
}

/// `Name (code)` for pickers, or the bare code when no preset
/// name is known (Python's Settings display rule).
pub fn language_display(code: &str) -> String {
    match language_name(code) {
        Some(name) => format!("{name} ({code})"),
        None => code.to_string(),
    }
}

/// The value the Translate page's target-language field shows
/// for a stored code (Python
/// `_translation_language_display_value`): the preset name
/// for the code (matched case-insensitively), the raw value
/// when unknown, and `English (en)` for an empty value.
pub fn language_display_value(raw: &str) -> String {
    let value = raw.trim();
    if value.is_empty() {
        return "English (en)".to_string();
    }
    let lowered = value.to_lowercase();
    for (name, code) in LANGUAGE_PRESETS {
        if code.to_lowercase() == lowered {
            return name.to_string();
        }
    }
    value.to_string()
}

/// Resolve the Translate page field's text back to a stored
/// code (Python `_translation_language_code_from_ui`): an
/// exact preset-name match wins, then a trailing
/// `(code)` suffix is unwrapped, then the text is the code
/// as typed. Empty resolves to English. Custom values are
/// preserved verbatim — never rewritten to a preset.
pub fn language_code_from_ui(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "en".to_string();
    }
    for (name, code) in LANGUAGE_PRESETS {
        if *name == raw {
            return code.to_string();
        }
    }
    if raw.ends_with(')')
        && let Some(open) = raw.rfind('(')
    {
        let maybe = raw[open + 1..raw.len() - 1].trim();
        if !maybe.is_empty() {
            return maybe.to_string();
        }
    }
    raw.to_string()
}

/// OpenRouter's public model list endpoint (Python
/// `_OPENROUTER_MODELS_URL`).
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Extract the sorted, deduplicated model ids from an
/// OpenRouter `/models` payload (Python
/// `list_openrouter_models`): `data[].id` strings, sorted
/// case-insensitively, duplicates removed case-insensitively
/// keeping the first spelling.
pub fn openrouter_models_from_payload(payload: &serde_json::Value) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    if let Some(items) = payload.get("data").and_then(|d| d.as_array()) {
        for item in items {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                let id = id.trim();
                if !id.is_empty() {
                    models.push(id.to_string());
                }
            }
        }
    }
    models.sort_by_key(|a| a.to_lowercase());
    let mut seen = std::collections::HashSet::new();
    models.retain(|m| seen.insert(m.to_lowercase()));
    models
}

/// The OpenRouter model combo's choices after a load
/// (Python `_on_openrouter_models_loaded`): the built-in
/// candidates first, then the loaded ids, deduplicated
/// case-insensitively preserving order.
pub fn openrouter_model_choices(loaded: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in default_models("openrouter")
        .iter()
        .map(|s| s.to_string())
        .chain(loaded.iter().cloned())
    {
        if seen.insert(item.to_lowercase()) {
            out.push(item);
        }
    }
    out
}

/// Load every model id OpenRouter offers (Python
/// `list_openrouter_models`, core/translation.py:331). The
/// API key is optional (the list is public); failures return
/// Python's error strings and never panic — the Settings
/// caller keeps its current choices on an error
/// (failure-safe model loading, Audit 8 S19).
pub fn list_openrouter_models(api_key: &str, timeout_s: u64) -> Result<Vec<String>, String> {
    let timeout = timeout_s.clamp(5, 60);
    let mut req = crate::http::agent()
        .get(OPENROUTER_MODELS_URL)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(timeout)))
        .http_status_as_error(false)
        .build()
        .header("Accept", "application/json")
        .header("HTTP-Referer", "https://github.com/serrebidev/BlindRSS")
        .header("X-Title", "BlindRSS");
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    let mut resp = req
        .call()
        .map_err(|e| format!("OpenRouter models request failed: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("OpenRouter models request failed: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("OpenRouter models request failed (HTTP {status})"));
    }
    let payload: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("OpenRouter models response was not valid JSON: {e}"))?;
    Ok(openrouter_models_from_payload(&payload))
}

/// Python `_looks_like_groq_key`: users often confuse Groq
/// and Grok keys; a `gsk_` key is a Groq key.
pub fn looks_like_groq_key(api_key: &str) -> bool {
    api_key.trim().to_lowercase().starts_with("gsk_")
}

/// The provider that actually serves a call: the configured
/// one, except Grok with a Groq-shaped key routes to Groq
/// (Python `_translate_chunk_grok`).
pub fn effective_provider<'a>(provider: &'a str, api_key: &str) -> &'a str {
    if provider == "grok" && looks_like_groq_key(api_key) {
        "groq"
    } else {
        provider
    }
}

/// The validated runtime translation configuration (Python
/// `_translation_runtime_config`): `Some` only when
/// translation is enabled AND the selected provider has an
/// API key. The automatic reader flows key off exactly this;
/// an unknown provider name clamps to grok, as in Python.
pub struct TranslationSettings {
    /// The selected provider (validated, pre-routing).
    pub provider: String,
    pub target_language: String,
    /// The configured model for the provider ("" = defaults).
    pub model: String,
    pub timeout_s: u64,
    pub chunk_chars: usize,
}

impl TranslationSettings {
    /// Python `_translation_fulltext_cache_suffix`: the
    /// variant identity separating cached translations by
    /// provider / target language / configured model.
    pub fn cache_suffix(&self) -> String {
        let provider = self.provider.trim().to_lowercase();
        let lang = self.target_language.trim().to_lowercase();
        let lang = if lang.is_empty() { "en" } else { &lang };
        let model = self.model.trim().to_lowercase();
        if model.is_empty() {
            format!("::tr[{provider}:{lang}]")
        } else {
            format!("::tr[{provider}:{lang}:{model}]")
        }
    }
}

pub fn settings_from_config(cfg: &crate::config::Config) -> Option<TranslationSettings> {
    if !cfg.get_bool("translation_enabled", false) {
        return None;
    }
    let mut provider = cfg
        .get_str("translation_provider", "grok")
        .trim()
        .to_lowercase();
    if !PROVIDERS.contains(&provider.as_str()) {
        provider = "grok".to_string();
    }
    let api_key = cfg
        .get_str(&format!("translation_{provider}_api_key"), "")
        .trim()
        .to_string();
    if api_key.is_empty() {
        return None;
    }
    let target = cfg
        .get_str("translation_target_language", "en")
        .trim()
        .to_string();
    let target = if target.is_empty() {
        "en".to_string()
    } else {
        target
    };
    let model = cfg
        .get_str(&format!("translation_{provider}_model"), "")
        .trim()
        .to_string();
    Some(TranslationSettings {
        provider,
        target_language: target,
        model,
        timeout_s: cfg.get_i64("translation_timeout_seconds", 45).clamp(5, 180) as u64,
        chunk_chars: cfg
            .get_i64("translation_chunk_chars", 3500)
            .clamp(500, 8000) as usize,
    })
}

fn timing_from_config(cfg: &crate::config::Config) -> (u64, usize) {
    (
        cfg.get_i64("translation_timeout_seconds", 45).clamp(5, 180) as u64,
        cfg.get_i64("translation_chunk_chars", 3500)
            .clamp(500, 8000) as usize,
    )
}

/// Split text into translation chunks (Python
/// `_iter_text_chunks`): contiguous slices of at most
/// `max_chars`, preferring paragraph, then line, then space
/// boundaries far enough into the slice. Concatenating the
/// chunks reproduces the input exactly.
pub fn chunk_text(text: &str) -> Vec<String> {
    chunk_text_with(text, CHUNK_CHARS)
}

fn chunk_text_with(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(200);
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    if n <= max_chars {
        return vec![text.to_string()];
    }
    // Last start index of `pat` fully inside chars[start..end].
    let rfind = |pat: &[char], start: usize, end: usize| -> Option<usize> {
        if pat.is_empty() || end > n || start + pat.len() > end {
            return None;
        }
        (start..=end - pat.len())
            .rev()
            .find(|&i| &chars[i..i + pat.len()] == pat)
    };
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < n {
        let mut end = (start + max_chars).min(n);
        if end < n {
            let split_at = rfind(&['\n', '\n'], start, end)
                .or_else(|| rfind(&['\n'], start, end))
                .or_else(|| rfind(&[' '], start, end));
            if let Some(split_at) = split_at
                && split_at > start + 200
            {
                end = split_at;
            }
        }
        if end <= start {
            end = (start + max_chars).min(n);
            if end <= start {
                break;
            }
        }
        let chunk: String = chars[start..end].iter().collect();
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        start = end;
    }
    chunks
}

/// Python `_extract_chat_completion_text`: the first choice's
/// message content — a plain string, or the joined `text` of
/// an array of parts — stripped. Empty when there is none.
pub fn extract_chat_text(payload: &serde_json::Value) -> String {
    let Some(content) = payload
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
    else {
        return String::new();
    };
    match content {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str())
                    && !t.is_empty()
                {
                    out.push_str(t);
                }
            }
            out.trim().to_string()
        }
        _ => String::new(),
    }
}

/// Python `_extract_gemini_completion_text`: every text part
/// of the first candidate joined, stripped.
pub fn extract_gemini_text(payload: &serde_json::Value) -> String {
    let Some(parts) = payload
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
    else {
        return String::new();
    };
    let mut out = String::new();
    for part in parts {
        if let Some(t) = part.get("text").and_then(|t| t.as_str())
            && !t.is_empty()
        {
            out.push_str(t);
        }
    }
    out.trim().to_string()
}

/// Python `_error_message_text`: the useful string inside an
/// error envelope (nested message/error/detail/code/type),
/// or "".
fn error_message_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            for key in ["message", "error", "detail", "code", "type"] {
                if let Some(nested) = map.get(key) {
                    let text = error_message_text(nested);
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
            value.to_string()
        }
        serde_json::Value::Array(items) => items
            .iter()
            .map(error_message_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python `_response_error_detail`: "HTTP {code}: {message}"
/// with the message whitespace-collapsed and capped.
fn response_error_detail(status: u16, body: &str) -> String {
    let mut msg = serde_json::from_str::<serde_json::Value>(body)
        .map(|v| error_message_text(&v))
        .unwrap_or_default();
    if msg.is_empty() {
        msg = body.trim().to_string();
    }
    msg = msg.split_whitespace().collect::<Vec<_>>().join(" ");
    if msg.chars().count() > 260 {
        msg = format!(
            "{}...",
            msg.chars().take(257).collect::<String>().trim_end()
        );
    }
    if status > 0 && !msg.is_empty() {
        format!("HTTP {status}: {msg}")
    } else if status > 0 {
        format!("HTTP {status}")
    } else {
        msg
    }
}

/// Python `_retryable_model_error` (HTTP half): only a
/// 400/403/404 whose message talks about a model being
/// unavailable advances to the next candidate.
fn retryable_model_error(status: u16, message: &str) -> bool {
    if !matches!(status, 400 | 403 | 404) {
        return false;
    }
    let low = message.to_lowercase();
    if !low.contains("model") {
        return false;
    }
    [
        "not found",
        "unknown",
        "invalid",
        "unavailable",
        "not available",
        "unsupported",
        "not allowed",
        "not permitted",
        "permission",
        "access",
        "entitled",
        "tier",
    ]
    .iter()
    .any(|t| low.contains(t))
}

/// Python `_gemini_empty_response_reason`: why Gemini
/// answered with no text (prompt feedback / finish reason).
fn gemini_empty_reason(payload: &serde_json::Value) -> String {
    if let Some(feedback) = payload.get("promptFeedback") {
        let detail = error_message_text(feedback);
        if !detail.is_empty() {
            return detail;
        }
    }
    if let Some(reason) = payload
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finishReason"))
        .and_then(|r| r.as_str())
        && reason != "STOP"
    {
        return reason.to_string();
    }
    String::new()
}

/// One provider call's failure. `retryable` is Python's
/// model-error verdict: only retryable failures advance to
/// the next candidate; everything else aborts the chunk.
struct CallError {
    message: String,
    retryable: bool,
}

impl CallError {
    fn fatal(message: String) -> Self {
        CallError {
            message,
            retryable: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn chat_once(
    label: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    target_language: &str,
    chunk: &str,
    timeout_s: u64,
    openrouter_headers: bool,
) -> Result<String, CallError> {
    let user_prompt = format!("Target language: {target_language}\n\nText to translate:\n{chunk}");
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
        .post(endpoint)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_s.max(5))))
        .http_status_as_error(false)
        .build()
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json");
    let req = if openrouter_headers {
        req.header("HTTP-Referer", "https://github.com/serrebidev/BlindRSS")
            .header("X-Title", "BlindRSS")
    } else {
        req
    };
    let mut resp = req
        .send(body.to_string())
        .map_err(|e| CallError::fatal(format!("{label} request: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| CallError::fatal(format!("{label} read: {e}")))?;
    if !(200..300).contains(&status) {
        let detail = response_error_detail(status, &text);
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .map(|v| error_message_text(&v))
            .unwrap_or_default();
        return Err(CallError {
            retryable: retryable_model_error(status, &message),
            message: if detail.is_empty() {
                format!("{label} request failed")
            } else {
                format!("{label} request failed ({detail})")
            },
        });
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CallError::fatal(format!("{label} response: {e}")))?;
    let translated = extract_chat_text(&value);
    if translated.is_empty() {
        return Err(CallError::fatal(format!(
            "{label} returned an empty translation response."
        )));
    }
    Ok(translated)
}

fn gemini_once(
    template: &str,
    api_key: &str,
    model: &str,
    target_language: &str,
    chunk: &str,
    timeout_s: u64,
) -> Result<String, CallError> {
    // Python's Gemini prompt is a single user text (no system
    // instruction), and the key rides both the header and the
    // `key` query parameter.
    let prompt = format!(
        "Translate this text into the target language.\nPreserve line breaks, headings, and overall formatting.\nReturn only translated text with no commentary.\n\nTarget language: {target_language}\n\nText to translate:\n{chunk}"
    );
    let mut url = template.replace("{model}", model);
    url.push_str(if url.contains('?') { "&key=" } else { "?key=" });
    url.push_str(api_key);
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [{"text": prompt}]}],
        "generationConfig": {"temperature": 0},
    });
    let mut resp = crate::http::agent()
        .post(&url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_s.max(5))))
        .http_status_as_error(false)
        .build()
        .header("x-goog-api-key", api_key)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.to_string())
        .map_err(|e| CallError::fatal(format!("Gemini request: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| CallError::fatal(format!("Gemini read: {e}")))?;
    if !(200..300).contains(&status) {
        let detail = response_error_detail(status, &text);
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .map(|v| error_message_text(&v))
            .unwrap_or_default();
        return Err(CallError {
            retryable: retryable_model_error(status, &message),
            message: if detail.is_empty() {
                "Gemini request failed".to_string()
            } else {
                format!("Gemini request failed ({detail})")
            },
        });
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CallError::fatal(format!("Gemini response: {e}")))?;
    let translated = extract_gemini_text(&value);
    if translated.is_empty() {
        let reason = gemini_empty_reason(&value);
        return Err(CallError::fatal(if reason.is_empty() {
            "Gemini returned an empty translation response.".to_string()
        } else {
            format!("Gemini returned an empty translation response ({reason})")
        }));
    }
    Ok(translated)
}

/// One chunk through one chat endpoint, walking the model
/// candidates with Python's fallback rule.
#[allow(clippy::too_many_arguments)]
fn translate_chunk_chat(
    label: &str,
    endpoint: &str,
    api_key: &str,
    models: &[String],
    target_language: &str,
    chunk: &str,
    timeout_s: u64,
    openrouter_headers: bool,
) -> Result<String, String> {
    let mut last_err = String::new();
    for model in models {
        match chat_once(
            label,
            endpoint,
            api_key,
            model,
            target_language,
            chunk,
            timeout_s,
            openrouter_headers,
        ) {
            Ok(t) => return Ok(t),
            Err(e) => {
                last_err = e.message;
                if !e.retryable {
                    break;
                }
            }
        }
    }
    Err(if last_err.is_empty() {
        format!("{label} translation failed")
    } else {
        last_err
    })
}

fn translate_chunk_gemini(
    template: &str,
    api_key: &str,
    models: &[String],
    target_language: &str,
    chunk: &str,
    timeout_s: u64,
) -> Result<String, String> {
    let mut last_err = String::new();
    for model in models {
        match gemini_once(template, api_key, model, target_language, chunk, timeout_s) {
            Ok(t) => return Ok(t),
            Err(e) => {
                last_err = e.message;
                if !e.retryable {
                    break;
                }
            }
        }
    }
    Err(if last_err.is_empty() {
        "Gemini translation failed".to_string()
    } else {
        last_err
    })
}

/// Translate `text` into `target_language` with the
/// configured provider. Config keys: translation_provider,
/// translation_<provider>_api_key, translation_<provider>_model.
pub fn translate_text(
    cfg: &crate::config::Config,
    text: &str,
    target_language: &str,
) -> Result<String, String> {
    translate_text_with_endpoints(cfg, text, target_language, &[], None)
}

/// `translate_text` with endpoint overrides — the test seam
/// for loopback provider servers. `chat_endpoints` replaces
/// the chat-completions URL list (Qwen walks all of them in
/// order; other chat providers use the first);
/// `gemini_template` replaces the generateContent URL
/// template (`{model}` placeholder).
pub fn translate_text_with_endpoints(
    cfg: &crate::config::Config,
    text: &str,
    target_language: &str,
    chat_endpoints: &[&str],
    gemini_template: Option<&str>,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }
    let selected = cfg
        .get_str("translation_provider", "grok")
        .trim()
        .to_lowercase();
    let selected = if PROVIDERS.contains(&selected.as_str()) {
        selected
    } else {
        "grok".to_string()
    };
    let api_key = cfg
        .get_str(&format!("translation_{selected}_api_key"), "")
        .trim()
        .to_string();
    if api_key.is_empty() {
        return Err(format!(
            "no API key configured for the {selected} translation provider"
        ));
    }
    let target = if target_language.trim().is_empty() {
        "en"
    } else {
        target_language.trim()
    };
    // Routing (S20): Grok + a Groq key is served by Groq —
    // its endpoint and default models — while a model the
    // user configured for the selected provider still wins.
    let effective = effective_provider(&selected, &api_key).to_string();
    let configured_model = cfg
        .get_str(&format!("translation_{selected}_model"), "")
        .trim()
        .to_string();
    let models: Vec<String> = if configured_model.is_empty() {
        default_models(&effective)
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![configured_model]
    };
    if models.is_empty() {
        return Err(format!("no {effective} translation models configured"));
    }
    let (timeout_s, chunk_chars) = timing_from_config(cfg);
    let truncated: String = text.chars().take(MAX_TOTAL_CHARS).collect();
    let mut out: Vec<String> = Vec::new();
    for chunk in chunk_text_with(&truncated, chunk_chars) {
        let translated = if effective == "gemini" {
            translate_chunk_gemini(
                gemini_template.unwrap_or(GEMINI_URL_TEMPLATE),
                &api_key,
                &models,
                target,
                &chunk,
                timeout_s,
            )
        } else {
            let label = provider_label(&effective);
            let override_owned: Vec<String> =
                chat_endpoints.iter().map(|s| s.to_string()).collect();
            let endpoint_list: Vec<String> = if override_owned.is_empty() {
                endpoints(&effective)
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                override_owned
            };
            // Qwen walks its endpoints in order per chunk
            // (Python `_translate_chunk_qwen`); every other
            // chat provider has exactly one.
            let mut last_err = String::new();
            let mut done: Option<String> = None;
            for endpoint in &endpoint_list {
                match translate_chunk_chat(
                    label,
                    endpoint,
                    &api_key,
                    &models,
                    target,
                    &chunk,
                    timeout_s,
                    effective == "openrouter",
                ) {
                    Ok(t) => {
                        done = Some(t);
                        break;
                    }
                    Err(e) => last_err = e,
                }
            }
            done.ok_or(last_err)
        };
        out.push(translated?);
    }
    // Python joins chunk results with no separator: the
    // chunker keeps boundary characters at the head of the
    // following chunk, so concatenation restores the text.
    Ok(out.join(""))
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
        // Python's chunker is lossless: concatenation is the
        // input (the join in translate relies on it).
        assert_eq!(chunks.concat(), text);
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
