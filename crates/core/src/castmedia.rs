//! Caster media preparation (Audit 8 Task 28, C01): the port of
//! `caster_engine.probe_media`, `_native_hls_url`, `guess_mime` /
//! `roku_mime`, the `_play_chromecast` load ladder from
//! `core/casting.py`, and the live-stall watchdog policy.
//!
//! A cast starts by PROBING the stream: what a URL claims to be and
//! what it is differ too often, so the caller-supplied MIME is never
//! trusted — magic bytes win over headers, MPEG-TS is relayed
//! through HLS, live loads are marked LIVE, and a load the receiver
//! rejects is retried the way Caster retries it (native HLS first,
//! then the relay, then the flipped stream type, then a conversion
//! relay), with a hard bound. This module is the decision layer;
//! the wire work lives in `castsession` / `castupnp` / `proxy`.

/// The User-Agent Caster's engine sends when it probes or talks to
/// receivers (`caster_engine` / `caster_devices` use it verbatim).
pub const CASTER_UA: &str = "caster/1.0";

/// Bytes read to classify a stream (`caster_engine.PROBE_BYTES`).
pub const PROBE_BYTES: usize = 2048;
/// Probe attempt budget (`caster_engine.PROBE_TOTAL_TIMEOUT`).
pub const PROBE_TOTAL_TIMEOUT: f64 = 8.0;
/// Longest one native-HLS segment may run before the sibling
/// playlist stops counting as the low-latency original
/// (`caster_engine.NATIVE_HLS_SEGMENT_LIMIT`).
pub const NATIVE_HLS_SEGMENT_LIMIT: f64 = 8.0;

/// Live watchdog cadence and bounds (`core/casting.py`).
pub const WATCHDOG_SECONDS: f64 = 5.0;
pub const CAST_STALL_TIMEOUT: f64 = 20.0;
pub const CAST_RECOVERY_INTERVAL: f64 = 30.0;
/// Load settle budget (`core/casting.py` LOAD_TIMEOUT).
pub const LOAD_TIMEOUT: f64 = 12.0;

pub const HLS_MIME: &str = "application/vnd.apple.mpegurl";
pub const TS_MIME: &str = "video/mp2t";

/// What the probe learned about a stream. `is_audio` is tri-state:
/// `None` means the evidence did not decide (Caster keeps the same
/// distinction — several decisions hinge on "unknown", not "video").
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub mime: String,
    pub is_audio: Option<bool>,
    pub is_live: bool,
}

impl ProbeResult {
    pub fn is_ts(&self) -> bool {
        self.mime == TS_MIME
    }
}

/// MPEG-TS sync bytes repeating at the packet stride. An aligned
/// buffer is proved at byte 0 with a single stride; a capture that
/// started mid-packet is searched at every offset and needs FOUR
/// consecutive strides (two hits coincide on random bytes about
/// once in 340, four effectively never). Port of
/// `caster_engine._looks_like_mpegts`.
pub fn looks_like_mpegts(head: &[u8]) -> bool {
    fn strides_at(head: &[u8], start: usize, stride: usize, hits: usize) -> bool {
        (0..hits).all(|k| head.get(start + k * stride) == Some(&0x47))
    }
    // Aligned: sync at byte 0 AND one full stride later.
    for stride in [188usize, 192] {
        if head.len() > stride && head[0] == 0x47 && head[stride] == 0x47 {
            return true;
        }
    }
    let limit = head.len().min(188);
    for start in 1..limit {
        if head[start] != 0x47 {
            continue;
        }
        for stride in [188usize, 192] {
            if head.len() > start + 3 * stride && strides_at(head, start, stride, 4) {
                return true;
            }
        }
    }
    false
}

/// The extension/mimetype subset of Python's `mimetypes` table the
/// probe paths consult. Unknown extensions guess nothing.
fn extension_mime(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next()?;
    if ext.len() == lower.len() {
        return None; // no dot at all
    }
    Some(match ext {
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/x-wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wma" => "audio/x-ms-wma",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "mpg" | "mpeg" => "video/mpeg",
        "ts" => "video/mp2t",
        "m3u8" | "m3u" => "application/vnd.apple.mpegurl",
        _ => return None,
    })
}

fn url_path(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => u.path().to_string(),
        Err(_) => url.split(['?', '#']).next().unwrap_or(url).to_string(),
    }
}

/// Port of `caster_engine.guess_mime`: the mimetype of the path,
/// audio/aac when the URL mentions aac, else the audio/mpeg guess.
pub fn guess_mime(url: &str) -> String {
    if let Some(mime) = extension_mime(&url_path(url)) {
        return mime.to_string();
    }
    if url.to_ascii_lowercase().contains("aac") {
        return "audio/aac".to_string();
    }
    "audio/mpeg".to_string()
}

/// Port of `caster_engine.roku_mime`: playlists are HLS, audio
/// files keep their type (wav normalized), everything else is
/// treated as MP4 video.
pub fn roku_mime(url: &str) -> String {
    let path = url_path(url).to_ascii_lowercase();
    if path.ends_with(".m3u8") || path.ends_with(".m3u") {
        return HLS_MIME.to_string();
    }
    if let Some(mime) = extension_mime(&path)
        && mime.starts_with("audio/")
    {
        return if mime == "audio/x-wav" {
            "audio/wav".to_string()
        } else {
            mime.to_string()
        };
    }
    "video/mp4".to_string()
}

const CT_AUDIO: &[&str] = &[
    "audio/mpeg",
    "audio/mp4",
    "audio/aac",
    "audio/ogg",
    "audio/wav",
    "audio/x-wav",
    "audio/flac",
    "audio/opus",
    "audio/x-ms-wma",
    "application/ogg",
];
const CT_VIDEO: &[&str] = &[
    "video/mp4",
    "video/mp2t",
    "video/webm",
    "video/x-matroska",
    "video/quicktime",
    "video/mpeg",
    "video/x-msvideo",
    "application/vnd.apple.mpegurl",
    "application/x-mpegurl",
];

fn is_playlist_ct(content_type: &str) -> bool {
    content_type == "application/vnd.apple.mpegurl" || content_type == "application/x-mpegurl"
}

/// Classify a probed head + Content-Type the way
/// `caster_engine.probe_media` does once it has bytes. Magic bytes
/// win over headers because IPTV servers lie about Content-Type.
pub fn classify_head(head: &[u8], content_type: Option<&str>, url: &str) -> (String, Option<bool>) {
    let ts = looks_like_mpegts(head);
    let magic_audio: Option<&str> = if head.starts_with(b"ID3")
        || head.starts_with(&[0xff, 0xfb])
        || head.starts_with(&[0xff, 0xf3])
    {
        Some("audio/mpeg")
    } else if head.starts_with(b"OggS") {
        Some("audio/ogg")
    } else if head.starts_with(b"fLaC") {
        Some("audio/flac")
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        Some("audio/wav")
    } else {
        None
    };
    if ts && magic_audio.is_none() {
        return (TS_MIME.to_string(), Some(false));
    }
    if let Some(mime) = magic_audio {
        return (mime.to_string(), Some(true));
    }
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return ("video/mp4".to_string(), Some(false));
    }
    let ct = content_type.unwrap_or("");
    if CT_AUDIO.contains(&ct) {
        return (ct.to_string(), Some(true));
    }
    if CT_VIDEO.contains(&ct) {
        return (ct.to_string(), Some(false));
    }
    if ts {
        return (TS_MIME.to_string(), Some(false));
    }
    let guess = guess_mime(url);
    let is_audio = if guess.starts_with("audio/") {
        Some(true)
    } else {
        None
    };
    (guess, is_audio)
}

/// One probe response as the transport saw it.
pub struct ProbeResponse {
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Probe a stream, deciding MIME / audio / live from its first
/// bytes. The transport and the backoff sleep are injected so the
/// retry policy is testable without a network; `probe_media` is
/// the production entry point.
pub fn probe_media_with(
    url: &str,
    fetch: &dyn Fn(&str) -> Result<ProbeResponse, String>,
    sleep: &dyn Fn(std::time::Duration),
) -> ProbeResult {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        // Local path (or file://): extension and magic bytes only;
        // a file is never live.
        let path = match url::Url::parse(url) {
            Ok(u) if u.scheme() == "file" => u
                .to_file_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => url.to_string(),
        };
        let head = std::fs::File::open(&path)
            .and_then(|mut f| {
                use std::io::Read;
                let mut buf = vec![0u8; PROBE_BYTES];
                let n = f.read(&mut buf)?;
                buf.truncate(n);
                Ok(buf)
            })
            .unwrap_or_default();
        let (mime, is_audio) = classify_head(&head, None, url);
        return ProbeResult {
            mime,
            is_audio,
            is_live: false,
        };
    }
    let mut best: Vec<u8> = Vec::new();
    let mut content_type: Option<String> = None;
    for attempt in 0..5 {
        if let Ok(resp) = fetch(url) {
            if resp.body.len() > best.len() {
                best = resp.body;
                content_type = resp.content_type;
            }
            // A playlist Content-Type decides the probe at once.
            if let Some(ct) = content_type.as_deref()
                && is_playlist_ct(ct)
            {
                return ProbeResult {
                    mime: ct.to_string(),
                    is_audio: Some(false),
                    is_live: true,
                };
            }
        }
        if best.len() > 3 * 188 {
            break; // enough bytes to prove TS; more cannot change it
        }
        if attempt < 4 {
            let backoff = 0.3 * 2f64.powi(attempt);
            sleep(std::time::Duration::from_secs_f64(backoff));
        }
    }
    if best.is_empty() && content_type.is_none() {
        // Every attempt failed: calling a stream VOD on no evidence
        // is the expensive mistake (a live channel cut off), so an
        // unanswered probe is treated as live.
        return ProbeResult {
            mime: guess_mime(url),
            is_audio: None,
            is_live: true,
        };
    }
    let (mime, is_audio) = classify_head(&best, content_type.as_deref(), url);
    let is_live = mime == TS_MIME;
    ProbeResult {
        mime,
        is_audio,
        is_live,
    }
}

/// Production probe: HTTP Range requests for the first
/// [`PROBE_BYTES`] with Caster's UA, per-attempt timeout shrinking
/// against the total budget.
pub fn probe_media(url: &str) -> ProbeResult {
    let started = std::time::Instant::now();
    let fetch = |u: &str| -> Result<ProbeResponse, String> {
        let remaining = PROBE_TOTAL_TIMEOUT - started.elapsed().as_secs_f64();
        let timeout = remaining.clamp(0.5, 3.0);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs_f64(timeout)))
            .build()
            .into();
        let mut resp = agent
            .get(u)
            .header("User-Agent", CASTER_UA)
            .header("Range", format!("bytes=0-{}", PROBE_BYTES - 1))
            .call()
            .map_err(|e| e.to_string())?;
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
        let body = resp
            .body_mut()
            .with_config()
            .limit(PROBE_BYTES as u64)
            .read_to_vec()
            .unwrap_or_default();
        Ok(ProbeResponse { content_type, body })
    };
    probe_media_with(url, &fetch, &|d| std::thread::sleep(d))
}

/// Whether a segment URL path has Caster's native-HLS shape:
/// `/(live/)?<seg>/<seg>/<digits>.ts` (`caster_engine` matches the
/// same shape with a regex over the path).
fn native_hls_candidate(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let path = parsed.path();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return None;
    }
    let last = segments[segments.len() - 1];
    let digits = last.strip_suffix(".ts")?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut candidate = parsed.clone();
    candidate.set_path(&format!("{}.m3u8", &path[..path.len() - 3]));
    Some(candidate.to_string())
}

/// Validate a fetched sibling playlist the way
/// `caster_engine._native_hls_url` does: a live playlist (no
/// ENDLIST) whose longest listed segment runs at most
/// [`NATIVE_HLS_SEGMENT_LIMIT`] seconds.
pub fn native_playlist_is_live(text: &str) -> bool {
    let mut lines = text.lines();
    match lines.next() {
        Some(first) if first.trim() == "#EXTM3U" => {}
        _ => return false,
    }
    let mut has_target = false;
    let mut has_segment_line = false;
    let mut longest = 0.0f64;
    let mut has_inf = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("#EXT-X-TARGETDURATION") {
            has_target = true;
        } else if line.starts_with("#EXT-X-ENDLIST") {
            return false; // a finished playlist is VOD, not the live original
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            has_inf = true;
            if let Some(dur) = rest
                .split(',')
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
            {
                longest = longest.max(dur);
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            has_segment_line = true;
        }
    }
    has_target && has_inf && has_segment_line && longest <= NATIVE_HLS_SEGMENT_LIMIT
}

/// Port of `caster_engine._native_hls_url`: some bundled IPTV
/// playlists serve one long TS segment per program chunk, and the
/// sibling playlist is the less-buffered original when it is
/// genuinely live with short segments. The text fetch is injected
/// for tests.
pub fn native_hls_url_with(
    url: &str,
    fetch_text: &dyn Fn(&str) -> Result<String, String>,
) -> Option<String> {
    let candidate = native_hls_candidate(url)?;
    let text = fetch_text(&candidate).ok()?;
    if native_playlist_is_live(&text) {
        Some(candidate)
    } else {
        None
    }
}

/// Production native-HLS check (5 s budget, 64 KiB cap, like
/// Caster).
pub fn native_hls_url(url: &str) -> Option<String> {
    native_hls_url_with(url, &|u| {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(5)))
            .build()
            .into();
        let mut resp = agent
            .get(u)
            .header("User-Agent", CASTER_UA)
            .call()
            .map_err(|e| e.to_string())?;
        resp.body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())
    })
}

/// One load the Chromecast chain wants attempted.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadAttempt {
    pub url: String,
    pub mime: String,
    /// "LIVE" or "BUFFERED" (CAST `streamType`).
    pub stream_type: &'static str,
    /// The source URL whose HLS relay this attempt plays through,
    /// when it plays through one — the relay must be stopped when
    /// the load is replaced or the cast ends.
    pub relay_source: Option<String>,
}

/// Relay URL maker injected into the load chain: (source URL,
/// live) -> the relay URL to load, or an error when no relay can
/// be started.
pub type RelayUrlMaker = dyn FnMut(&str, bool) -> Result<String, String>;

/// The `_play_chromecast` retry ladder as a state machine: the
/// caller attempts each load, reports whether the receiver settled,
/// and the chain produces the next attempt or gives up. The relay
/// URL maker is injected: production starts the real relay in
/// `proxy`, tests substitute a fake URL.
pub struct ChromecastLoadChain {
    source_url: String,
    probe: ProbeResult,
    native: Option<String>,
    relay_url: Box<RelayUrlMaker>,
    /// Index of the next retry step: 0 = native-relay fallback,
    /// 1 = stream-type flip, 2 = conversion relay.
    step: u8,
    current: Option<LoadAttempt>,
    relay_used: bool,
    used_native: bool,
}

impl ChromecastLoadChain {
    pub fn new(
        source_url: &str,
        probe: ProbeResult,
        native: Option<String>,
        relay_url: impl FnMut(&str, bool) -> Result<String, String> + 'static,
    ) -> Self {
        Self {
            source_url: source_url.to_string(),
            probe,
            native,
            relay_url: Box::new(relay_url),
            step: 0,
            current: None,
            relay_used: false,
            used_native: false,
        }
    }

    fn stream_type(&self) -> &'static str {
        if self.probe.is_live {
            "LIVE"
        } else {
            "BUFFERED"
        }
    }

    fn flipped(&self) -> &'static str {
        if self.current.as_ref().map(|c| c.stream_type) == Some("LIVE") {
            "BUFFERED"
        } else {
            "LIVE"
        }
    }

    fn relay_attempt(&mut self, live: bool, stream_type: &'static str) -> Option<LoadAttempt> {
        match (self.relay_url)(&self.source_url, live) {
            Ok(url) => {
                self.relay_used = true;
                Some(LoadAttempt {
                    url,
                    mime: HLS_MIME.to_string(),
                    stream_type,
                    relay_source: Some(self.source_url.clone()),
                })
            }
            Err(_) => None,
        }
    }

    /// The first load to attempt.
    pub fn first(&mut self) -> Option<LoadAttempt> {
        let attempt = if self.probe.is_ts() && self.probe.is_live {
            if let Some(native) = self.native.clone() {
                self.used_native = true;
                LoadAttempt {
                    url: native,
                    mime: HLS_MIME.to_string(),
                    stream_type: "LIVE",
                    relay_source: None,
                }
            } else {
                self.relay_attempt(true, "LIVE")?
            }
        } else if self.probe.is_ts() {
            // A transport stream the receiver cannot demux: relay
            // it (remux to HLS) from the start.
            self.relay_attempt(self.probe.is_live, self.stream_type())?
        } else {
            LoadAttempt {
                url: self.source_url.clone(),
                mime: self.probe.mime.clone(),
                stream_type: self.stream_type(),
                relay_source: None,
            }
        };
        self.current = Some(attempt.clone());
        Some(attempt)
    }

    /// The current attempt did not settle; produce the next one,
    /// or `None` when the ladder is exhausted (Caster raises
    /// "did not start playing" at that point). The steps run in
    /// `_play_chromecast`'s order: a rejected native playlist falls
    /// back to the live relay, then the stream type flips
    /// LIVE <-> BUFFERED on the same URL, then — only for a direct
    /// load of on-demand media — the conversion relay. At most
    /// four loads are ever attempted for one cast.
    pub fn advance(&mut self) -> Option<LoadAttempt> {
        while self.step < 3 {
            let step = self.step;
            self.step += 1;
            match step {
                0 => {
                    if self.used_native {
                        let attempt = self.relay_attempt(true, "LIVE")?;
                        self.current = Some(attempt.clone());
                        return Some(attempt);
                    }
                }
                1 => {
                    let cur = self.current.clone()?;
                    let attempt = LoadAttempt {
                        stream_type: self.flipped(),
                        ..cur
                    };
                    self.current = Some(attempt.clone());
                    return Some(attempt);
                }
                _ => {
                    if !self.relay_used
                        && !self.probe.is_live
                        && self.current.as_ref().map(|c| c.url.as_str())
                            == Some(self.source_url.as_str())
                    {
                        let attempt = self.relay_attempt(false, "BUFFERED")?;
                        self.current = Some(attempt.clone());
                        return Some(attempt);
                    }
                }
            }
        }
        None
    }

    /// Source URL of the relay the current attempt plays through,
    /// if any (teardown bookkeeping for the caller).
    pub fn active_relay_source(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.relay_source.as_deref())
    }
}

/// The `_play_upnp` target decision: TS and non-HTTP sources go
/// through the relay; an HTTP source plays directly with the
/// PROBED mime. Returns (play_url, mime, relay_source).
pub fn prepare_upnp_target(
    url: &str,
    probe: &ProbeResult,
    relay_url: &dyn Fn(&str, bool) -> Result<String, String>,
) -> Result<(String, String, Option<String>), String> {
    if probe.is_ts() || !(url.starts_with("http://") || url.starts_with("https://")) {
        let play = relay_url(url, probe.is_live)?;
        Ok((play, HLS_MIME.to_string(), Some(url.to_string())))
    } else {
        Ok((url.to_string(), probe.mime.clone(), None))
    }
}

/// The `_play_roku` target decision: Roku has no MPEG-TS, so TS is
/// relayed; everything else plays directly with `roku_mime`.
/// Returns (play_url, mime, relay_source).
pub fn prepare_roku_target(
    url: &str,
    probe: &ProbeResult,
    relay_url: &dyn Fn(&str, bool) -> Result<String, String>,
) -> Result<(String, String, Option<String>), String> {
    if probe.is_ts() {
        let play = relay_url(url, probe.is_live)?;
        Ok((play, HLS_MIME.to_string(), Some(url.to_string())))
    } else {
        Ok((url.to_string(), roku_mime(url), None))
    }
}

/// The live-stall watchdog from `core/casting.py`
/// (`_recover_live_cast`), as a clock-injected policy: it is armed
/// only for loads recorded as live; a stall is
/// [`CAST_STALL_TIMEOUT`] seconds with neither a new status nor
/// position movement on a live session, an idle live session is
/// recovered, a pause is the user's and is left alone, and
/// recoveries are at most one per [`CAST_RECOVERY_INTERVAL`].
#[derive(Debug, Default)]
pub struct LiveWatchdog {
    armed: bool,
    progress: Option<(i64, f64)>,
    progress_since: f64,
    last_recovery: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogAction {
    None,
    Reload,
}

impl LiveWatchdog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a load as live (or clear the record for a VOD load).
    pub fn arm(&mut self, live: bool) {
        self.armed = live;
        self.progress = None;
    }

    pub fn disarm(&mut self) {
        self.armed = false;
        self.progress = None;
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// One status observation at time `now` (seconds, any
    /// monotonic clock). `state` is the receiver's player state;
    /// `session_id` / `position` identify progress.
    pub fn note(
        &mut self,
        state: Option<&str>,
        session_id: Option<i64>,
        position: Option<f64>,
        now: f64,
    ) -> WatchdogAction {
        if !self.armed {
            return WatchdogAction::None;
        }
        match state {
            Some("PLAYING") | Some("BUFFERING") => {
                let key = (session_id.unwrap_or(0), position.unwrap_or(0.0));
                match self.progress {
                    None => {
                        self.progress = Some(key);
                        self.progress_since = now;
                        WatchdogAction::None
                    }
                    Some(prev) if prev != key => {
                        self.progress = Some(key);
                        self.progress_since = now;
                        WatchdogAction::None
                    }
                    Some(_) => {
                        if now - self.progress_since >= CAST_STALL_TIMEOUT {
                            self.recover(now)
                        } else {
                            WatchdogAction::None
                        }
                    }
                }
            }
            Some("IDLE") => self.recover(now),
            // A pause (or anything else) is the user's, not a stall.
            _ => {
                self.progress = None;
                WatchdogAction::None
            }
        }
    }

    fn recover(&mut self, now: f64) -> WatchdogAction {
        if self.last_recovery > 0.0 && now - self.last_recovery < CAST_RECOVERY_INTERVAL {
            return WatchdogAction::None;
        }
        self.last_recovery = now;
        self.progress = None;
        WatchdogAction::Reload
    }
}
