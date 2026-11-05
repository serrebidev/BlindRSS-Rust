//! Shared yt-dlp recovery policy (Audit 8 Task 22, M16).
//!
//! A faithful port of the bounded retry policy the Python app
//! shares between playback resolution (`gui/player.py`) and
//! downloads (`gui/mainframe.py`), with the policy tables in
//! `core/discovery.py`: VLC-friendly audio format selection,
//! YouTube player-client pools, cookie attempts (anonymous base,
//! exported cookie file, installed-browser sources), no futile
//! retries (uncookieable errors and Chromium DPAPI failures skip
//! the attempts that cannot succeed), wider-client-pool and
//! recovery-URL phases with impersonation, and Python's error
//! choice (the anonymous attempt's error is the one reported).
//!
//! Deliberate non-ports, recorded in docs/PARITY.md: Python's
//! final pytubefix phase and its hidden-browser visitor-data
//! bootstrap have no Rust analogue; the phases here end at the
//! impersonated recovery-URL phase. yt-dlp itself stays a
//! wrapped executable run through `dependency::run_tool`.
//!
//! Nothing in this module logs cookie contents, cookie file
//! paths' contents, or signed URLs.

/// `_YTDLP_VLC_AUDIO_FORMAT` (gui/player.py): m4a first for VLC,
/// then any best audio, then a small video, then worst.
pub const VLC_AUDIO_FORMAT: &str =
    "bestaudio[ext=m4a]/bestaudio[ext=mp4]/bestaudio/best[height<=480]/worst";

/// `_YTDLP_SOUNDCLOUD_AUDIO_FORMAT`: SoundCloud's progressive
/// 128k MP3 is both the highest quality and a direct seekable
/// HTTP stream (no HLS transcode).
pub const SOUNDCLOUD_AUDIO_FORMAT: &str =
    "bestaudio[protocol^=http][ext=mp3]/bestaudio[ext=mp3]/bestaudio/best";

/// Primary YouTube player-client pool (`YOUTUBE_PLAYER_CLIENTS`).
pub const YOUTUBE_PLAYER_CLIENTS: [&str; 2] = ["default", "android_vr"];

/// Wider fallback pool (`YOUTUBE_PLAYER_CLIENTS_FALLBACK`).
pub const YOUTUBE_PLAYER_CLIENTS_FALLBACK: [&str; 11] = [
    "default",
    "android_vr",
    "web_safari",
    "web_embedded",
    "web_music",
    "tv_simply",
    "tv_downgraded",
    "tv",
    "ios",
    "mweb",
    "web",
];

/// The `--extractor-args` value for a client pool
/// (`youtube_player_client_arg`): visitor data is appended only
/// when it is safe to place on a command line (no whitespace or
/// control characters); Python drops it otherwise.
pub fn youtube_player_client_arg(clients: &[&str], visitor_data: Option<&str>) -> String {
    let mut out = format!("youtube:player_client={}", clients.join(","));
    if let Some(vd) = visitor_data {
        let vd = vd.trim();
        if !vd.is_empty() && vd.chars().all(|c| !c.is_whitespace() && !c.is_control()) {
            out.push_str(";visitor_data=");
            out.push_str(vd);
        }
    }
    out
}

/// Lowercased host of a URL ("" when unparseable).
pub fn url_host(url: &str) -> String {
    url::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default()
}

/// Path component of a URL ("" when unparseable).
pub fn url_path(url: &str) -> String {
    url::Url::parse(url.trim())
        .map(|u| u.path().to_string())
        .unwrap_or_default()
}

/// Query component of a URL ("" when absent/unparseable).
pub fn url_query(url: &str) -> String {
    url::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.query().map(|q| q.to_string()))
        .unwrap_or_default()
}

/// True for YouTube *page* URLs (`is_youtube_page_url`): watch,
/// shorts, live, embed, youtu.be and the music front-end. Media
/// (googlevideo) URLs are not page URLs.
pub fn is_youtube_page_url(url: &str) -> bool {
    let host = url_host(url);
    matches!(
        host.as_str(),
        "youtube.com"
            | "www.youtube.com"
            | "m.youtube.com"
            | "music.youtube.com"
            | "www.youtube-nocookie.com"
            | "youtube-nocookie.com"
            | "youtu.be"
    )
}

fn is_soundcloud_url(url: &str) -> bool {
    let host = url_host(url);
    host == "soundcloud.com" || host.ends_with(".soundcloud.com")
}

/// The audio format selector for a target URL
/// (`_ytdlp_audio_format_for`).
pub fn vlc_audio_format(url: &str) -> &'static str {
    if is_soundcloud_url(url) {
        SOUNDCLOUD_AUDIO_FORMAT
    } else {
        VLC_AUDIO_FORMAT
    }
}

fn youtube_video_id(url: &str) -> Option<String> {
    crate::sponsorblock::youtube_video_id(url)
}

/// `normalize_ytdlp_single_item_url`: a YouTube radio mix
/// auto-plays a generated station; when the link asks for the
/// radio of the current video, collapse it to the plain watch
/// URL so resolution targets the single item.
pub fn normalize_ytdlp_single_item_url(url: &str) -> String {
    let text = url.trim();
    if text.is_empty() {
        return String::new();
    }
    let Some(vid) = youtube_video_id(text) else {
        return text.to_string();
    };
    let host = url_host(text);
    let is_watch =
        (host.contains("youtube") && url_path(text).contains("watch")) || host == "youtu.be";
    if !is_watch {
        return text.to_string();
    }
    let mut list_value = String::new();
    let mut start_radio = String::new();
    for pair in url_query(text).split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "list" => list_value = v.to_string(),
            "start_radio" => start_radio = v.to_string(),
            _ => {}
        }
    }
    if list_value.starts_with("RD") && matches!(start_radio.as_str(), "1" | "true" | "yes") {
        return format!("https://www.youtube.com/watch?v={vid}");
    }
    text.to_string()
}

/// `youtube_single_item_recovery_urls`: alternate front-ends
/// for the same video, tried when the watch page refuses every
/// client pool. Python requires the id to match
/// `[A-Za-z0-9_-]{6,}`; `sponsorblock::youtube_video_id` already
/// restricts the charset, so only the length is checked here.
pub fn youtube_single_item_recovery_urls(url: &str) -> Vec<String> {
    let canonical = normalize_ytdlp_single_item_url(url);
    let host = url_host(&canonical);
    if !(host == "youtube.com" || host.ends_with(".youtube.com")) {
        return Vec::new();
    }
    if !url_path(&canonical).contains("watch") {
        return Vec::new();
    }
    let Some(vid) = youtube_video_id(&canonical) else {
        return Vec::new();
    };
    if vid.len() < 6 {
        return Vec::new();
    }
    vec![
        format!("https://music.youtube.com/watch?v={vid}"),
        format!("https://www.youtube-nocookie.com/embed/{vid}"),
    ]
}

/// One cookie attempt in Python's attempt list: the anonymous
/// base attempt, the exported cookie file, or one installed
/// browser source (formatted for `--cookies-from-browser`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CookieAttempt {
    Base,
    CookieFile(String),
    Browser(String),
}

impl CookieAttempt {
    /// The yt-dlp arguments this attempt adds.
    pub fn args(&self) -> Vec<String> {
        match self {
            CookieAttempt::Base => Vec::new(),
            CookieAttempt::CookieFile(path) => {
                vec!["--cookies".to_string(), path.clone()]
            }
            CookieAttempt::Browser(source) => {
                vec!["--cookies-from-browser".to_string(), source.clone()]
            }
        }
    }

    pub fn is_browser(&self) -> bool {
        matches!(self, CookieAttempt::Browser(_))
    }

    /// The browser keyword for a browser attempt (the part
    /// before `:` in `browser:profile` form).
    pub fn browser(&self) -> Option<&str> {
        match self {
            CookieAttempt::Browser(source) => Some(source.split(':').next().unwrap_or(source)),
            _ => None,
        }
    }
}

/// `get_ytdlp_cookie_sources` + `_build_cookie_sources`: the
/// ordered `--cookies-from-browser` values. Windows builds the
/// list from the profile directories that actually exist (Brave
/// first, Chromium family, Edge, Firefox family; LibreWolf rides
/// the firefox keyword with an explicit Profiles path); macOS
/// and Linux let yt-dlp locate each default profile. When the
/// Windows list ends up empty, Python falls back to the bare
/// keywords.
pub fn cookie_sources() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |value: String, out: &mut Vec<String>| {
        if !out.contains(&value) {
            out.push(value);
        }
    };
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let roaming = std::env::var("APPDATA").unwrap_or_default();
        let join = |base: &str, parts: &[&str]| -> std::path::PathBuf {
            let mut p = std::path::PathBuf::from(base);
            for part in parts {
                p.push(part);
            }
            p
        };
        // (keyword, user-data dir, is the default install)
        let table: Vec<(&str, std::path::PathBuf, bool)> = vec![
            (
                "brave",
                join(&local, &["BraveSoftware", "Brave-Browser", "User Data"]),
                true,
            ),
            (
                "brave",
                join(
                    &local,
                    &["BraveSoftware", "Brave-Browser-Beta", "User Data"],
                ),
                false,
            ),
            (
                "brave",
                join(
                    &local,
                    &["BraveSoftware", "Brave-Browser-Nightly", "User Data"],
                ),
                false,
            ),
            (
                "chrome",
                join(&local, &["Google", "Chrome", "User Data"]),
                true,
            ),
            (
                "chrome",
                join(&local, &["Google", "Chrome Beta", "User Data"]),
                false,
            ),
            (
                "chrome",
                join(&local, &["Google", "Chrome SxS", "User Data"]),
                false,
            ),
            ("chromium", join(&local, &["Chromium", "User Data"]), true),
            ("vivaldi", join(&local, &["Vivaldi", "User Data"]), true),
            (
                "edge",
                join(&local, &["Microsoft", "Edge", "User Data"]),
                true,
            ),
            (
                "edge",
                join(&local, &["Microsoft", "Edge Beta", "User Data"]),
                false,
            ),
            (
                "edge",
                join(&local, &["Microsoft", "Edge SxS", "User Data"]),
                false,
            ),
            (
                "opera",
                join(&roaming, &["Opera Software", "Opera Stable"]),
                true,
            ),
        ];
        for (name, path, is_default) in table {
            if !path.as_os_str().is_empty() && path.is_dir() {
                if is_default {
                    add(name.to_string(), &mut out);
                } else {
                    add(format!("{name}:{}", path.to_string_lossy()), &mut out);
                }
            }
        }
        let firefox_profiles = join(&roaming, &["Mozilla", "Firefox", "Profiles"]);
        if !roaming.is_empty() && firefox_profiles.is_dir() {
            add("firefox".to_string(), &mut out);
        }
        for base in [&roaming, &local] {
            if base.is_empty() {
                continue;
            }
            let lw = join(base, &["librewolf", "Profiles"]);
            if lw.is_dir() {
                add(format!("firefox:{}", lw.to_string_lossy()), &mut out);
                break;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for name in [
            "brave", "chrome", "chromium", "vivaldi", "edge", "opera", "firefox",
        ] {
            add(name.to_string(), &mut out);
        }
    }
    if out.is_empty() {
        for name in ["brave", "chrome", "chromium", "edge", "firefox"] {
            add(name.to_string(), &mut out);
        }
    }
    out
}

/// `is_chromium_ytdlp_cookie_source`: Chromium-family stores
/// are the ones App-Bound Encryption (DPAPI) can lock.
pub fn is_chromium_cookie_source(arg: &str) -> bool {
    let browser = arg
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        browser.as_str(),
        "brave" | "chrome" | "chromium" | "vivaldi" | "edge" | "opera" | "whale"
    )
}

/// `is_ytdlp_dpapi_cookie_error`.
pub fn is_dpapi_cookie_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("dpapi") || lower.contains("failed to decrypt with dpapi")
}

/// `_is_ytdlp_uncookieable_error`: errors no cookie can fix.
/// YouTube page URLs are excluded (their messages often claim
/// the URL is unsupported when the real problem is a bot/login
/// gate, and the cookie attempts exist for exactly that gate);
/// cookie/login wording means cookies may still help.
pub fn is_uncookieable_error(text: &str, url: &str) -> bool {
    if is_youtube_page_url(url) {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    for marker in ["cookie", "log in", "logged-in", "logged in", "sign in"] {
        if lower.contains(marker) {
            return false;
        }
    }
    lower.contains("please report this issue") || lower.contains("unsupported url")
}

fn is_bloomberg_url(url: &str) -> bool {
    url_host(url).contains("bloomberg.")
}

/// The full attempt list (`get_ytdlp_cookie_attempts`): base,
/// cookie file when one is configured and exists, then the
/// browser sources. Bloomberg hosts take the cookie file FIRST
/// (their anonymous path is a hard paywall).
pub fn cookie_attempts(cookie_file: Option<&str>) -> Vec<CookieAttempt> {
    cookie_attempts_for_url(cookie_file, "")
}

/// Bloomberg-aware attempt order for a specific target URL.
pub fn cookie_attempts_for_url(cookie_file: Option<&str>, url: &str) -> Vec<CookieAttempt> {
    let file = cookie_file
        .map(str::trim)
        .filter(|p| !p.is_empty() && std::path::Path::new(p).is_file());
    let mut out = Vec::new();
    if is_bloomberg_url(url) {
        if let Some(f) = file {
            out.push(CookieAttempt::CookieFile(f.to_string()));
        }
        out.push(CookieAttempt::Base);
    } else {
        out.push(CookieAttempt::Base);
        if let Some(f) = file {
            out.push(CookieAttempt::CookieFile(f.to_string()));
        }
    }
    out.extend(cookie_sources().into_iter().map(CookieAttempt::Browser));
    out
}

/// The reduced attempt list of the fallback phases
/// (`cookie_attempts` limited to base + cookie file): the wider
/// phases do not re-run the browser sources.
pub fn fallback_cookie_attempts(cookie_file: Option<&str>) -> Vec<CookieAttempt> {
    let file = cookie_file
        .map(str::trim)
        .filter(|p| !p.is_empty() && std::path::Path::new(p).is_file());
    let mut out = vec![CookieAttempt::Base];
    if let Some(f) = file {
        out.push(CookieAttempt::CookieFile(f.to_string()));
    }
    out
}

/// One recovery phase: a target URL, the client pool to use,
/// whether only the reduced attempt list applies, and whether
/// the attempt impersonates Chrome.
#[derive(Clone, Debug)]
pub struct Phase {
    pub target_url: String,
    pub clients: &'static [&'static str],
    pub fallback_attempts: bool,
    pub impersonate: bool,
}

/// The phase list for a target (`recovery_phases` in the Python
/// workers): primary pool + full attempts; for YouTube, the
/// fallback pool + reduced attempts; then one impersonated
/// phase per recovery URL. The URL is normalized first (radio
/// mixes collapse to the single item).
pub fn recovery_phases(url: &str) -> Vec<Phase> {
    let target = normalize_ytdlp_single_item_url(url);
    let mut phases = vec![Phase {
        target_url: target.clone(),
        clients: &YOUTUBE_PLAYER_CLIENTS,
        fallback_attempts: false,
        impersonate: false,
    }];
    if is_youtube_page_url(&target) {
        phases.push(Phase {
            target_url: target.clone(),
            clients: &YOUTUBE_PLAYER_CLIENTS_FALLBACK,
            fallback_attempts: true,
            impersonate: false,
        });
        for recovery in youtube_single_item_recovery_urls(&target) {
            phases.push(Phase {
                target_url: recovery,
                clients: &YOUTUBE_PLAYER_CLIENTS_FALLBACK,
                fallback_attempts: true,
                impersonate: true,
            });
        }
    }
    phases
}

/// The outcome of one attempt, as reported by the caller's
/// runner: success, or a failure carrying the attempt's error
/// text (full stderr — the classification helpers scan it) and
/// whether the failure is fatal to the whole operation
/// (a subprocess timeout in Python stops all phases).
pub enum AttemptOutcome<T> {
    Success(T),
    Failed { error: String, fatal: bool },
}

/// Session state Python's workers carry across attempts.
struct RecoveryState {
    dpapi_seen: bool,
    skip_browser_cookies: bool,
    base_err: String,
    last_err: String,
}

/// Run the bounded recovery phases for `url`, invoking
/// `attempt(phase, cookie_attempt)` for each permitted attempt
/// until one succeeds. Applies Python's futile-retry rules:
/// browser attempts are skipped after an uncookieable base
/// error; Chromium sources are skipped after a DPAPI failure
/// (Firefox sources continue); a fatal failure stops all
/// phases. The returned error is the anonymous base attempt's
/// error when there is one (Python reports that one — a cookie
/// attempt's failure must not mask the real cause), with the
/// DPAPI explanation appended when DPAPI was seen.
pub fn run_recovery<T>(
    url: &str,
    cookie_file: Option<&str>,
    mut attempt: impl FnMut(&Phase, &CookieAttempt) -> AttemptOutcome<T>,
) -> Result<T, String> {
    let phases = recovery_phases(url);
    let mut state = RecoveryState {
        dpapi_seen: false,
        skip_browser_cookies: false,
        base_err: String::new(),
        last_err: String::new(),
    };
    for (index, phase) in phases.iter().enumerate() {
        let attempts = if phase.fallback_attempts {
            fallback_cookie_attempts(cookie_file)
        } else if index == 0 {
            cookie_attempts_for_url(cookie_file, &phase.target_url)
        } else {
            cookie_attempts(cookie_file)
        };
        for att in &attempts {
            if att.is_browser() {
                if state.skip_browser_cookies {
                    continue;
                }
                if state.dpapi_seen && att.browser().is_some_and(is_chromium_cookie_source) {
                    continue;
                }
            }
            match attempt(phase, att) {
                AttemptOutcome::Success(value) => return Ok(value),
                AttemptOutcome::Failed { error, fatal } => {
                    if att.is_browser() && is_dpapi_cookie_error(&error) {
                        state.dpapi_seen = true;
                    }
                    if matches!(att, CookieAttempt::Base) && state.base_err.is_empty() {
                        if index == 0 && is_uncookieable_error(&error, url) {
                            state.skip_browser_cookies = true;
                        }
                        state.base_err = error.clone();
                    }
                    state.last_err = error;
                    if fatal {
                        return Err(final_error(&state));
                    }
                }
            }
        }
    }
    Err(final_error(&state))
}

fn final_error(state: &RecoveryState) -> String {
    let mut err = if !state.base_err.is_empty() {
        state.base_err.clone()
    } else if !state.last_err.is_empty() {
        state.last_err.clone()
    } else {
        "yt-dlp failed".to_string()
    };
    if state.dpapi_seen {
        // Python mainframe's wording, verbatim.
        err.push_str(
            "\n\nBrowser cookies could not be read on this Windows session (Chromium App-Bound Encryption). If this item needs a login, export a cookies.txt from your browser and set it in Settings.",
        );
    }
    err
}
