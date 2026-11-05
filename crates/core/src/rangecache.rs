//! Range-cache proxy (Python `core/range_cache_proxy.py`,
//! Batch 36).
//!
//! A loopback HTTP server that sits between the player and
//! remote media. The origin is probed once (a one-byte range
//! request) to learn the real total length and whether ranges
//! are supported; GET /media?id=<sid> then serves byte ranges
//! from an on-disk segment cache, fetching cache misses from
//! the origin in the background of the response and keeping
//! them as chunk files (`<start>-<end>.bin`, twelve-digit
//! zero-padded — the same layout Python writes, so a cache
//! directory is shared safely between the two apps). A
//! background downloader keeps filling the cache from the
//! play head so seeks land on local bytes. Signed URLs that
//! expire mid-playback (megaphone/podtrac chains) are
//! re-resolved from the original URL and the fetch retried
//! once.
//!
//! Why the player wants this (Python's reasons, unchanged):
//! a truthful Content-Length and real 206 answers stop the
//! backend from adopting a served window as the whole file
//! (an hour-long episode showing as four minutes), googlevideo
//! seeks stop clamping to a wrong duration, and replays hit
//! disk instead of the network.
//!
//! Deliberate simplifications vs Python (see docs/PARITY.md):
//! the server is this process's second listener (127.0.0.1
//! only, an ephemeral port kept for the process lifetime)
//! rather than a restartable object; responses close the
//! connection per request like the rest of the local proxy;
//! per-request timeouts ride the dedicated agent's global
//! timeout instead of Python's split connect/read values.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::Digest;

const PROBE_WAIT: Duration = Duration::from_secs(3);
const PROBE_RESOLVE_WAIT: Duration = Duration::from_secs(20);
/// Python `_PROBE_FORBIDDEN_RETRY_S`: pause before the one
/// probe retry after a 401/403.
const PROBE_FORBIDDEN_RETRY: Duration = Duration::from_secs(1);
const INLINE_PREFETCH_CAP: u64 = 16 * 1024 * 1024;
/// Python `_BG_MAX_REQUEST_BYTES`: no single background range
/// request may exceed 10 MiB — past that, googlevideo answers
/// larger ranges at ~31 KB/s after a delay, while many small
/// ranges are served fast (Audit 8 Task 22, M04).
pub const BG_MAX_REQUEST_BYTES: u64 = 10 * 1024 * 1024;

/// The background worker's per-request target (Python:
/// `min(bootstrap or chunk, _BG_MAX_REQUEST_BYTES)`): the
/// first pass aims at the bootstrap depth, later passes at
/// the steady-state chunk, both clamped to the 10 MiB cap so
/// the configured depth is filled incrementally.
pub fn background_request_target(first: bool, bootstrap_bytes: u64, chunk_bytes: u64) -> u64 {
    let target = if first { bootstrap_bytes } else { chunk_bytes };
    target.min(BG_MAX_REQUEST_BYTES)
}
const TRACKING_HOSTS: [&str; 11] = [
    "op3.dev",
    "pdst.fm",
    "pscrb.fm",
    "chrt.fm",
    "chtbl.com",
    "podtrac.com",
    "mgln.ai",
    "blubrry.com",
    "podcasts.apple.com",
    "anchor.fm",
    "spotify.com",
];

// ---------------------------------------------------------------------
// Segment math — pure helpers, direct ports of Python's.

/// Merge overlapping or adjacent inclusive ranges.
pub fn merge_segments(mut segs: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    if segs.is_empty() {
        return segs;
    }
    segs.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(segs.len());
    let (mut cs, mut ce) = segs[0];
    for (s, e) in segs.into_iter().skip(1) {
        if s <= ce.saturating_add(1) {
            ce = ce.max(e);
        } else {
            out.push((cs, ce));
            cs = s;
            ce = e;
        }
    }
    out.push((cs, ce));
    out
}

/// Normalize on-disk chunk segments. Segments correspond 1:1
/// with cache files; do NOT merge here, or metadata may point
/// at non-existent files.
pub fn normalize_segments(segs: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    let mut set: Vec<(u64, u64)> = segs.into_iter().filter(|(s, e)| e >= s).collect();
    set.sort_unstable();
    set.dedup();
    set
}

/// The parts of [start, end] not covered by `have`.
pub fn missing_segments(have: &[(u64, u64)], start: u64, end: u64) -> Vec<(u64, u64)> {
    if start > end {
        return Vec::new();
    }
    let have = merge_segments(have.to_vec());
    let mut missing = Vec::new();
    let mut cur = start;
    for (s, e) in have {
        if e < cur {
            continue;
        }
        if s > end {
            break;
        }
        if s > cur {
            missing.push((cur, end.min(s - 1)));
        }
        cur = cur.max(e.saturating_add(1));
        if cur > end {
            break;
        }
    }
    if cur <= end {
        missing.push((cur, end));
    }
    missing
}

/// Parse a Content-Range header ("bytes 0-0/12345" or
/// "bytes 0-0/*") into (start, end, Option<total>).
pub fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total_raw) = rest.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let total = if total_raw.trim() == "*" {
        None
    } else {
        Some(total_raw.trim().parse().ok()?)
    };
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total))
}

/// Parse a client Range header against the known total.
/// Open-ended ranges leave the end unset so the caller can
/// clamp to a smaller inline window. Mirrors Python's
/// `_parse_range_header` (single range, no suffix form).
pub fn parse_range_header(value: &str, total_length: Option<u64>) -> Option<(u64, Option<u64>)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    if start_s.is_empty() {
        return None;
    }
    let start: u64 = start_s.parse().ok()?;
    if end_s.is_empty() {
        return Some((start, None));
    }
    let mut end: u64 = end_s.parse().ok()?;
    if end < start {
        end = start;
    }
    if let Some(total) = total_length {
        end = end.min(total.saturating_sub(1).max(start));
    }
    Some((start, Some(end)))
}

pub fn sha256_hex(s: &str) -> String {
    let digest = sha2::Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------
// Configuration

#[derive(Clone, Debug)]
pub struct RangeCacheConfig {
    pub cache_dir: PathBuf,
    pub prefetch_bytes: u64,
    pub inline_window_bytes: u64,
    pub initial_burst_bytes: u64,
    pub initial_inline_prefetch_bytes: u64,
    pub background_download: bool,
    pub background_chunk_bytes: u64,
    pub debug: bool,
}

impl Default for RangeCacheConfig {
    fn default() -> Self {
        Self::from_app_config(&crate::config::Config::default())
    }
}

impl RangeCacheConfig {
    /// Python's constructor clamps plus the player call site's
    /// config keys and defaults (player.py `_maybe_range_cache_url`
    /// passes initial_burst_kb default 65536, the constructor's
    /// own 32768 never wins when the player drives).
    pub fn from_app_config(cfg: &crate::config::Config) -> Self {
        let dir = cfg.get_str("range_cache_dir", "");
        let cache_dir = if dir.trim().is_empty() {
            std::env::temp_dir().join("BlindRSS_streamcache")
        } else {
            PathBuf::from(dir)
        };
        let kb =
            |key: &str, default: i64| -> u64 { (cfg.get_i64(key, default)).max(0) as u64 * 1024 };
        RangeCacheConfig {
            cache_dir,
            prefetch_bytes: kb("range_cache_prefetch_kb", 16384).max(512 * 1024),
            inline_window_bytes: kb("range_cache_inline_window_kb", 1024).max(256 * 1024),
            initial_burst_bytes: kb("range_cache_initial_burst_kb", 65536).max(4 * 1024 * 1024),
            initial_inline_prefetch_bytes: kb("range_cache_initial_inline_prefetch_kb", 1024)
                .min(INLINE_PREFETCH_CAP),
            background_download: cfg.get_bool("range_cache_background_download", true),
            background_chunk_bytes: kb("range_cache_background_chunk_kb", 8192).max(1024 * 1024),
            debug: cfg.get_bool("range_cache_debug", false),
        }
    }
}

// ---------------------------------------------------------------------
// Cache entry

/// An opened origin range response: status, Content-Range,
/// and an owned body reader.
struct RangeFetch {
    status: u16,
    content_range: String,
    reader: ureq::BodyReader<'static>,
}

struct EntryState {
    segments: Vec<(u64, u64)>,
    total_length: Option<u64>,
    content_type: String,
    range_supported: Option<bool>,
    real_url: Option<String>,
    last_access: Instant,
    bg_cursor: u64,
    bootstrap_done: bool,
    last_req_start: u64,
    bg_running: bool,
}

struct Entry {
    url: String,
    headers: Vec<(String, String)>,
    dir: PathBuf,
    cfg: RangeCacheConfig,
    state: Mutex<EntryState>,
    probe_status: Mutex<u8>, // 0 not started, 1 running, 2 done
    probe_cv: Condvar,
    reresolve_lock: Mutex<Instant>,
    bg_stop: Arc<AtomicBool>,
}

impl Entry {
    fn new(url: &str, headers: Vec<(String, String)>, cfg: RangeCacheConfig) -> Entry {
        let dir = cfg.cache_dir.join(sha256_hex(url));
        let _ = std::fs::create_dir_all(&dir);
        let entry = Entry {
            url: url.to_string(),
            headers,
            dir,
            cfg,
            state: Mutex::new(EntryState {
                segments: Vec::new(),
                total_length: None,
                content_type: "application/octet-stream".to_string(),
                range_supported: None,
                real_url: None,
                last_access: Instant::now(),
                bg_cursor: 0,
                bootstrap_done: false,
                last_req_start: 0,
                bg_running: false,
            }),
            probe_status: Mutex::new(0),
            probe_cv: Condvar::new(),
            reresolve_lock: Mutex::new(Instant::now() - Duration::from_secs(60)),
            bg_stop: Arc::new(AtomicBool::new(false)),
        };
        entry.load_existing_segments();
        entry
    }

    fn touch(&self) {
        self.state.lock().unwrap().last_access = Instant::now();
    }

    fn chunk_path(&self, start: u64, end: u64) -> PathBuf {
        self.dir.join(format!("{start:012}-{end:012}.bin"))
    }

    fn load_existing_segments(&self) {
        let mut segs = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for item in rd.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                let Some(stem) = name.strip_suffix(".bin") else {
                    continue;
                };
                let Some((a, b)) = stem.split_once('-') else {
                    continue;
                };
                if let (Ok(s), Ok(e)) = (a.parse::<u64>(), b.parse::<u64>())
                    && e >= s
                {
                    segs.push((s, e));
                }
            }
        }
        self.state.lock().unwrap().segments = normalize_segments(segs);
    }

    fn remove_segment(&self, seg: (u64, u64)) {
        let mut st = self.state.lock().unwrap();
        st.segments.retain(|s| *s != seg);
    }

    /// Move a finished temp chunk into place and record it.
    fn finalize_chunk(&self, tmp: &std::path::Path, start: u64, end: u64) {
        if end < start {
            let _ = std::fs::remove_file(tmp);
            return;
        }
        let final_path = self.chunk_path(start, end);
        let mut st = self.state.lock().unwrap();
        if final_path.exists() {
            let _ = std::fs::remove_file(tmp);
            return;
        }
        if std::fs::rename(tmp, &final_path).is_ok() {
            st.segments.push((start, end));
            st.segments = normalize_segments(std::mem::take(&mut st.segments));
        } else {
            let _ = std::fs::remove_file(tmp);
        }
    }

    fn origin_headers(&self, range: Option<(u64, u64)>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.headers.clone();
        let has = |headers: &[(String, String)], name: &str| {
            headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
        };
        if !has(&out, "User-Agent") {
            out.push(("User-Agent".into(), crate::http::user_agent()));
        }
        if !has(&out, "Accept") {
            out.push(("Accept".into(), "*/*".into()));
        }
        out.retain(|(k, _)| !k.eq_ignore_ascii_case("Accept-Encoding"));
        out.push(("Accept-Encoding".into(), "identity".into()));
        if let Some((s, e)) = range {
            out.retain(|(k, _)| !k.eq_ignore_ascii_case("Range"));
            out.push(("Range".into(), format!("bytes={s}-{e}")));
        }
        out
    }

    fn target_url(&self) -> String {
        let st = self.state.lock().unwrap();
        st.real_url.clone().unwrap_or_else(|| self.url.clone())
    }

    // -- Probe ----------------------------------------------------

    /// Whether the probe state is settled (Python's probe
    /// early-exit): support known AND (total known OR support
    /// is false). A probe round that was refused (401/403)
    /// leaves support UNKNOWN — not settled — so the next
    /// probe runs again (Audit 8 Task 22, M03).
    fn probe_settled(&self) -> bool {
        let st = self.state.lock().unwrap();
        st.range_supported.is_some()
            && (st.total_length.is_some() || st.range_supported == Some(false))
    }

    /// Single-flight probe (Python's per-entry probe). Learns
    /// the total length, the content type, whether ranges are
    /// supported, and the final URL after redirects.
    fn probe(self: &Arc<Self>) {
        {
            let mut status = self.probe_status.lock().unwrap();
            loop {
                match *status {
                    2 => {
                        if self.probe_settled() {
                            return;
                        }
                        // The last round was refused: support
                        // is still unknown, so probe again
                        // (Python clears _probe_done here).
                        *status = 0;
                    }
                    1 => {
                        while *status == 1 {
                            status = self.probe_cv.wait(status).unwrap();
                        }
                        return;
                    }
                    _ => {
                        *status = 1;
                        break;
                    }
                }
            }
        }
        let finish = |entry: &Arc<Self>| {
            let mut status = entry.probe_status.lock().unwrap();
            *status = 2;
            entry.probe_cv.notify_all();
        };
        {
            let st = self.state.lock().unwrap();
            if st.range_supported.is_some()
                && (st.total_length.is_some() || st.range_supported == Some(false))
            {
                drop(st);
                finish(self);
                return;
            }
        }
        let target = self.target_url();
        // Python retries a refused (401/403) probe once after
        // a short pause: googlevideo can refuse a freshly
        // minted URL's first probe, then serve ranges a moment
        // later. A round that still ends refused leaves range
        // support UNKNOWN below, so a later request probes
        // again instead of the session being poisoned (M03).
        let mut resp =
            crate::http_headers::media_get(&self.url, &target, &self.origin_headers(Some((0, 0))));
        if let Ok(r) = &resp {
            let status = r.status().as_u16();
            if status == 401 || status == 403 {
                std::thread::sleep(PROBE_FORBIDDEN_RETRY);
                resp = crate::http_headers::media_get(
                    &self.url,
                    &target,
                    &self.origin_headers(Some((0, 0))),
                );
            }
        }
        match resp {
            Err(_) => {
                let mut st = self.state.lock().unwrap();
                st.range_supported = Some(false);
                st.total_length = None;
            }
            Ok(resp) => {
                use ureq::ResponseExt as _;
                let status = resp.status().as_u16();
                let final_url = resp.get_uri().to_string();
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let content_range = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let content_length = resp
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                if self.cfg.debug {
                    // S17 diagnostic (Python's HTTP seek probe
                    // log line): URLs are redacted — signed
                    // media URLs carry credentials in their
                    // query strings.
                    let accept_ranges = resp
                        .headers()
                        .get("accept-ranges")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    eprintln!(
                        "[range-cache] probe url={} final={} status={} accept_ranges={} content_range={} content_type={} content_length={}",
                        redact_url_for_log(&self.url),
                        redact_url_for_log(&final_url),
                        status,
                        accept_ranges,
                        content_range,
                        content_type,
                        content_length.map(|v| v.to_string()).unwrap_or_default()
                    );
                }
                let mut st = self.state.lock().unwrap();
                if !final_url.is_empty() {
                    st.real_url = Some(final_url);
                }
                if !content_type.is_empty() {
                    st.content_type = content_type;
                }
                if status == 206 {
                    if let Some((_, _, total)) = parse_content_range(&content_range)
                        && total.is_some()
                    {
                        st.total_length = total;
                    }
                    st.range_supported = Some(true);
                } else if status == 200 {
                    st.range_supported = Some(false);
                    if let Some(cl) = content_length
                        && cl > 0
                    {
                        st.total_length = Some(cl);
                    }
                } else if status == 401 || status == 403 {
                    // Refused even after the retry: Python
                    // leaves range support UNKNOWN (it does not
                    // guess "unsupported") and the next request
                    // probes again (M03).
                    if self.cfg.debug {
                        eprintln!(
                            "[range-cache] probe refused (status {status}) for {}; range support left unknown, next request probes again",
                            redact_url_for_log(&self.url)
                        );
                    }
                } else {
                    st.range_supported = Some(false);
                }
            }
        }
        finish(self);
    }

    /// Wait for the initial probe before building a response
    /// (Python's await_probe): a short wait, then a longer one
    /// while the total is still unknown — answering the player
    /// without the real total makes it adopt the served window
    /// as the entire file.
    fn await_probe(self: &Arc<Self>) {
        let done = |e: &Arc<Self>| *e.probe_status.lock().unwrap() == 2;
        let known = |e: &Arc<Self>| {
            let st = e.state.lock().unwrap();
            st.total_length.is_some() || st.range_supported == Some(false)
        };
        if !done(self) {
            let status = self.probe_status.lock().unwrap();
            let _ = self.probe_cv.wait_timeout(status, PROBE_WAIT).unwrap();
        }
        if known(self) {
            return;
        }
        if !done(self) {
            let status = self.probe_status.lock().unwrap();
            let _ = self
                .probe_cv
                .wait_timeout(status, PROBE_RESOLVE_WAIT)
                .unwrap();
        }
        if !known(self) {
            // Not done yet, OR done but the last probe was
            // refused (support unknown): probe (again) inline,
            // exactly Python's await_probe (M03).
            self.probe();
        }
    }

    /// Refresh an expired signed real_url by re-following
    /// redirects from the original URL. Throttled +
    /// single-flight (Python's `_refresh_real_url`).
    fn refresh_real_url(&self) -> bool {
        let mut last = self.reresolve_lock.lock().unwrap();
        if last.elapsed() < Duration::from_secs(4) {
            return self.state.lock().unwrap().real_url.is_some();
        }
        *last = Instant::now();
        drop(last);
        match crate::http_headers::media_get(
            &self.url,
            &self.url,
            &self.origin_headers(Some((0, 0))),
        ) {
            Ok(resp) => {
                use ureq::ResponseExt as _;
                let status = resp.status().as_u16();
                let final_url = resp.get_uri().to_string();
                if (status == 200 || status == 206) && !final_url.is_empty() {
                    self.state.lock().unwrap().real_url = Some(final_url);
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// GET a range from the origin with Python's rescue: a
    /// transport error or an expiry-shaped status
    /// (401/403/404/410) refreshes the signed URL and retries
    /// once.
    fn open_range(&self, start: u64, end: u64) -> Option<RangeFetch> {
        let mut target = self.target_url();
        for attempt in 0..2 {
            match crate::http_headers::media_get(
                &self.url,
                &target,
                &self.origin_headers(Some((start, end))),
            ) {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if attempt == 0 && [401, 403, 404, 410].contains(&status) {
                        drop(resp);
                        if self.refresh_real_url() {
                            target = self.target_url();
                            continue;
                        }
                        return None;
                    }
                    let content_range = resp
                        .headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    return Some(RangeFetch {
                        status,
                        content_range,
                        reader: resp.into_body().into_reader(),
                    });
                }
                Err(_) => {
                    if attempt == 0 && self.refresh_real_url() {
                        target = self.target_url();
                        continue;
                    }
                    return None;
                }
            }
        }
        None
    }

    // -- Fetch + cache ---------------------------------------------

    /// Fetch [start, end] from the origin and store it as a
    /// chunk file. Partial downloads over 4 KB are kept as
    /// shorter chunks (Python's `_fetch_range`).
    fn fetch_range(self: &Arc<Self>, start: u64, end: u64, abort: &dyn Fn() -> bool) -> bool {
        self.probe();
        if self.state.lock().unwrap().range_supported == Some(false) {
            return false;
        }
        {
            let st = self.state.lock().unwrap();
            let have = merge_segments(st.segments.clone());
            if have.iter().any(|(s, e)| *s <= start && end <= *e) {
                return true;
            }
        }
        let Some(mut fetch) = self.open_range(start, end) else {
            return false;
        };
        if fetch.status == 200 {
            self.state.lock().unwrap().range_supported = Some(false);
            return false;
        }
        if fetch.status != 206 {
            return false;
        }
        let (mut served_start, mut served_end) = (start, end);
        if let Some((a, b, total)) = parse_content_range(&fetch.content_range) {
            served_start = a;
            served_end = b;
            if total.is_some() {
                self.state.lock().unwrap().total_length = total;
            }
        }
        let expected = served_end
            .checked_sub(served_start)
            .map(|d| d + 1)
            .unwrap_or(0);
        if expected == 0 {
            return false;
        }
        // The entry dir is created at construction, but the
        // cache is shared with the Python app (dual-run): an
        // external cleanup can remove it under a live entry,
        // and a bare File::create would fail ENOENT.
        let _ = std::fs::create_dir_all(&self.dir);
        let tmp = self.dir.join(format!(
            "tmp_fetch_{}_{}.part",
            std::process::id(),
            served_start
        ));
        let mut written: u64 = 0;
        let mut aborted = false;
        let write_result = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            let reader = &mut fetch.reader;
            let mut buf = [0u8; 65536];
            loop {
                if abort() {
                    aborted = true;
                    break;
                }
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let take = ((expected - written).min(n as u64)) as usize;
                f.write_all(&buf[..take])?;
                written += take as u64;
                if written >= expected {
                    break;
                }
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        if aborted || written != expected {
            if written > 4096 {
                let actual_end = served_start + written - 1;
                self.finalize_chunk(&tmp, served_start, actual_end);
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
            return false;
        }
        self.finalize_chunk(&tmp, served_start, served_end);
        true
    }

    fn best_segment_covering(&self, offset: u64) -> Option<(u64, u64)> {
        let st = self.state.lock().unwrap();
        let mut best: Option<(u64, u64)> = None;
        for (s, e) in &st.segments {
            if *s <= offset && offset <= *e && best.is_none_or(|(_, be)| *e > be) {
                best = Some((*s, *e));
            }
        }
        best
    }

    fn next_segment_start_after(&self, offset: u64) -> Option<u64> {
        let st = self.state.lock().unwrap();
        st.segments
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| *s > offset)
            .min()
    }

    /// Stream cached bytes [start, end] to `w`; returns the
    /// last byte offset written. Errors on a cache miss.
    fn stream_cached_range_to(
        &self,
        mut cur: u64,
        end: u64,
        w: &mut dyn Write,
    ) -> std::io::Result<u64> {
        while cur <= end {
            let Some((s, e)) = self.best_segment_covering(cur) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cache miss while streaming",
                ));
            };
            let part_end = e.min(end);
            let path = self.chunk_path(s, e);
            let mut f = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(err) => {
                    self.remove_segment((s, e));
                    return Err(err);
                }
            };
            f.seek(SeekFrom::Start(cur - s))?;
            let mut remaining = part_end - cur + 1;
            let mut buf = [0u8; 65536];
            while remaining > 0 {
                let take = remaining.min(buf.len() as u64) as usize;
                let n = f.read(&mut buf[..take])?;
                if n == 0 {
                    self.remove_segment((s, e));
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "cache read failed",
                    ));
                }
                w.write_all(&buf[..n])?;
                remaining -= n as u64;
            }
            cur = part_end + 1;
        }
        Ok(end)
    }

    /// Stream [start, end] from the origin to `w` while caching
    /// it (Python's `stream_origin_range_to_and_cache`).
    /// Returns the last byte offset successfully written, or
    /// start - 1 on failure. Partial progress is kept as a
    /// shorter chunk.
    fn stream_origin_range_to_and_cache(
        self: &Arc<Self>,
        start: u64,
        end: u64,
        w: &mut dyn Write,
    ) -> i64 {
        let fail = start as i64 - 1;
        if end < start {
            return fail;
        }
        self.probe();
        if self.state.lock().unwrap().range_supported == Some(false) {
            return fail;
        }
        let Some(mut fetch) = self.open_range(start, end) else {
            return fail;
        };
        if fetch.status == 200 {
            self.state.lock().unwrap().range_supported = Some(false);
            return fail;
        }
        if fetch.status != 206 {
            return fail;
        }
        let mut served_start = start;
        let mut served_end = end;
        let mut skip: u64 = 0;
        if let Some((a, b, total)) = parse_content_range(&fetch.content_range) {
            if total.is_some() {
                self.state.lock().unwrap().total_length = total;
            }
            if a > start || b < start {
                return fail;
            }
            if a < start {
                skip = start - a;
                served_start = start;
            } else {
                served_start = a;
            }
            served_end = b.min(end);
        }
        let expected = served_end
            .checked_sub(served_start)
            .map(|d| d + 1)
            .unwrap_or(0);
        if expected == 0 {
            return fail;
        }
        // As in fetch_chunk: the shared cache dir can be
        // removed externally under a live entry (dual-run
        // cleanup); re-ensure it before creating the temp file.
        let _ = std::fs::create_dir_all(&self.dir);
        let tmp = self
            .dir
            .join(format!("tmp_{}_{}.part", std::process::id(), served_start));
        let mut written: u64 = 0;
        let mut client_gone = false;
        let stream_result = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            let reader = &mut fetch.reader;
            let mut buf = [0u8; 65536];
            let mut remaining_skip = skip;
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let mut chunk = &buf[..n];
                if remaining_skip > 0 {
                    if (chunk.len() as u64) <= remaining_skip {
                        remaining_skip -= chunk.len() as u64;
                        continue;
                    }
                    chunk = &chunk[remaining_skip as usize..];
                    remaining_skip = 0;
                }
                let room = expected - written;
                if (chunk.len() as u64) > room {
                    chunk = &chunk[..room as usize];
                }
                if chunk.is_empty() {
                    break;
                }
                // Cache first, so a slow client still warms the disk.
                f.write_all(chunk)?;
                if w.write_all(chunk).is_err() {
                    client_gone = true;
                    break;
                }
                written += chunk.len() as u64;
                if written >= expected {
                    break;
                }
            }
            Ok(())
        })();
        if stream_result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return fail;
        }
        if client_gone || written != expected {
            if written > 0 {
                let actual_end = served_start + written - 1;
                self.finalize_chunk(&tmp, served_start, actual_end);
                return actual_end as i64;
            }
            let _ = std::fs::remove_file(&tmp);
            return fail;
        }
        self.finalize_chunk(&tmp, served_start, served_end);
        served_end as i64
    }

    // -- Background download ----------------------------------------

    fn advance_bg_cursor(st: &mut EntryState) {
        let have = merge_segments(st.segments.clone());
        let mut cur = st.bg_cursor;
        loop {
            let mut advanced = false;
            for (s, e) in &have {
                if *s <= cur && cur <= *e {
                    cur = e + 1;
                    advanced = true;
                    break;
                }
                if *s > cur {
                    break;
                }
            }
            if !advanced {
                break;
            }
        }
        st.bg_cursor = cur;
    }

    fn maybe_start_background_download(self: &Arc<Self>) {
        if !self.cfg.background_download {
            return;
        }
        {
            let mut st = self.state.lock().unwrap();
            if st.bg_running {
                return;
            }
            st.bg_running = true;
        }
        self.bg_stop.store(false, Ordering::SeqCst);
        let entry = self.clone();
        std::thread::spawn(move || {
            entry.background_run();
            entry.state.lock().unwrap().bg_running = false;
        });
    }

    fn stop_background(&self) {
        self.bg_stop.store(true, Ordering::SeqCst);
    }

    fn background_run(self: &Arc<Self>) {
        self.probe();
        if self.state.lock().unwrap().range_supported == Some(false) {
            return;
        }
        let bootstrap_bytes = self
            .cfg
            .initial_burst_bytes
            .max(self.cfg.background_chunk_bytes);
        let jump_threshold = self.cfg.background_chunk_bytes.max(4 * 1024 * 1024);
        let mut first = true;
        while !self.bg_stop.load(Ordering::SeqCst) {
            {
                let st = self.state.lock().unwrap();
                if st.last_access.elapsed() > Duration::from_secs(120) {
                    return;
                }
            }
            let (ms, me) = {
                let mut st = self.state.lock().unwrap();
                if st.bootstrap_done {
                    let req = st.last_req_start;
                    if req.abs_diff(st.bg_cursor) > jump_threshold {
                        st.bg_cursor = req;
                    }
                }
                Self::advance_bg_cursor(&mut st);
                let start = st.bg_cursor;
                if let Some(total) = st.total_length
                    && start >= total
                {
                    drop(st);
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                let chunk_target = background_request_target(
                    first,
                    bootstrap_bytes,
                    self.cfg.background_chunk_bytes,
                );
                let mut end = start.saturating_add(chunk_target).saturating_sub(1);
                if let Some(total) = st.total_length {
                    end = end.min(total.saturating_sub(1));
                }
                let miss = missing_segments(&st.segments, start, end);
                match miss.first() {
                    Some(&(a, b)) => (a, b),
                    None => {
                        st.bg_cursor = end + 1;
                        drop(st);
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            };
            let ref_req = self.state.lock().unwrap().last_req_start;
            let stop = self.bg_stop.clone();
            let entry_ref = self.clone();
            let abort = move || {
                if stop.load(Ordering::SeqCst) {
                    return true;
                }
                let cur_req = entry_ref.state.lock().unwrap().last_req_start;
                cur_req.abs_diff(ref_req) > 2 * 1024 * 1024
            };
            if !self.fetch_range(ms, me, &abort) {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            first = false;
            {
                let mut st = self.state.lock().unwrap();
                Self::advance_bg_cursor(&mut st);
                if !st.bootstrap_done && st.bg_cursor >= self.cfg.initial_burst_bytes {
                    st.bootstrap_done = true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---------------------------------------------------------------------
// The proxy server (one per process, loopback only)

struct Proxy {
    cfg: Mutex<RangeCacheConfig>,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    port: Mutex<Option<u16>>,
    delivery: Mutex<Option<(bool, Instant)>>,
}

static PROXY: OnceLock<Arc<Proxy>> = OnceLock::new();

fn proxy() -> Arc<Proxy> {
    PROXY
        .get_or_init(|| {
            Arc::new(Proxy {
                cfg: Mutex::new(RangeCacheConfig::default()),
                entries: Mutex::new(HashMap::new()),
                port: Mutex::new(None),
                delivery: Mutex::new(None),
            })
        })
        .clone()
}

fn map_dir(cfg: &RangeCacheConfig) -> PathBuf {
    cfg.cache_dir.join("mappings")
}

fn save_mapping(cfg: &RangeCacheConfig, sid: &str, url: &str, headers: &[(String, String)]) {
    let dir = map_dir(cfg);
    let _ = std::fs::create_dir_all(&dir);
    let mut map = serde_json::Map::new();
    for (k, v) in headers {
        map.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    let payload = serde_json::json!({"url": url, "headers": map});
    let tmp = dir.join(format!("{sid}.json.tmp"));
    if std::fs::write(&tmp, payload.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(format!("{sid}.json")));
    }
}

fn load_mapping(cfg: &RangeCacheConfig, sid: &str) -> Option<(String, Vec<(String, String)>)> {
    let text = std::fs::read_to_string(map_dir(cfg).join(format!("{sid}.json"))).ok()?;
    let obj: serde_json::Value = serde_json::from_str(&text).ok()?;
    let url = obj.get("url")?.as_str()?.to_string();
    if url.is_empty() {
        return None;
    }
    let mut headers = Vec::new();
    if let Some(map) = obj.get("headers").and_then(|h| h.as_object()) {
        for (k, v) in map {
            if let Some(v) = v.as_str() {
                headers.push((k.clone(), v.to_string()));
            }
        }
    }
    Some((url, headers))
}

impl Proxy {
    fn get_or_create_entry(
        &self,
        sid: &str,
        url: &str,
        headers: &[(String, String)],
    ) -> Arc<Entry> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(ent) = entries.get(sid) {
            return ent.clone();
        }
        let cfg = self.cfg.lock().unwrap().clone();
        let _ = std::fs::create_dir_all(&cfg.cache_dir);
        let ent = Arc::new(Entry::new(url, headers.to_vec(), cfg));
        entries.insert(sid.to_string(), ent.clone());
        ent
    }

    fn entry_for(&self, sid: &str) -> Option<Arc<Entry>> {
        if let Some(ent) = self.entries.lock().unwrap().get(sid) {
            return Some(ent.clone());
        }
        let cfg = self.cfg.lock().unwrap().clone();
        let (url, headers) = load_mapping(&cfg, sid)?;
        Some(self.get_or_create_entry(sid, &url, &headers))
    }

    fn ensure_started(self: &Arc<Self>) -> Option<u16> {
        if let Some(port) = *self.port.lock().unwrap() {
            return Some(port);
        }
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        *self.port.lock().unwrap() = Some(port);
        let proxy = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let p = proxy.clone();
                        std::thread::spawn(move || handle(stream, &p));
                    }
                    Err(_) => break,
                }
            }
        });
        Some(port)
    }

    fn base_url(self: &Arc<Self>) -> Option<String> {
        self.ensure_started()
            .map(|port| format!("http://127.0.0.1:{port}"))
    }

    /// Drop entries idle past the limit once the table grows
    /// past the cap (Python's prune, same numbers).
    fn prune(&self, max_entries: usize, max_idle: Duration) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() <= max_entries {
            return;
        }
        let mut by_age: Vec<(String, Instant)> = entries
            .iter()
            .map(|(sid, ent)| (sid.clone(), ent.state.lock().unwrap().last_access))
            .collect();
        by_age.sort_by_key(|(_, t)| *t);
        for (sid, last) in by_age {
            if entries.len() <= max_entries {
                break;
            }
            if last.elapsed() < max_idle {
                continue;
            }
            if let Some(ent) = entries.remove(&sid) {
                ent.stop_background();
            }
        }
    }
}

/// Register a URL and return its local range-cache URL
/// (Python's `proxify`). The id is a stable hash of the URL
/// plus the header set, so player retries reuse one cache.
/// The probe starts in the background; the request handler
/// waits for it when the player asks for bytes.
pub fn proxify(url: &str, headers: &[(String, String)], skip_redirect_resolve: bool) -> String {
    if url.is_empty() {
        return url.to_string();
    }
    let p = proxy();
    let Some(base) = p.base_url() else {
        return url.to_string();
    };
    let mut sorted: Vec<&(String, String)> = headers.iter().collect();
    sorted.sort_by_key(|a| a.0.to_lowercase());
    let id_src = format!(
        "{url}\n{}",
        sorted
            .iter()
            .map(|(k, v)| format!("{}:{v}", k.to_lowercase()))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let sid = sha256_hex(&id_src)[..24].to_string();
    let cfg = p.cfg.lock().unwrap().clone();
    save_mapping(&cfg, &sid, url, headers);
    let ent = p.get_or_create_entry(&sid, url, headers);
    if skip_redirect_resolve {
        ent.state.lock().unwrap().real_url = Some(url.to_string());
    }
    let probe_entry = ent.clone();
    std::thread::spawn(move || probe_entry.probe());
    p.prune(20, Duration::from_secs(1800));
    format!("{base}/media?id={sid}")
}

/// Can this machine actually deliver a response body written
/// after the headers (what media serving does)? One cached
/// local round trip against /health-split; a positive verdict
/// holds ten minutes, a negative one two (Python's
/// `can_deliver_body`).
pub fn can_deliver_body() -> bool {
    let p = proxy();
    {
        let delivery = p.delivery.lock().unwrap();
        if let Some((ok, at)) = *delivery {
            let ttl = if ok { 600 } else { 120 };
            if at.elapsed() < Duration::from_secs(ttl) {
                return ok;
            }
        }
    }
    let ok = (|| -> bool {
        let Some(port) = p.ensure_started() else {
            return true; // not started: nothing to say yet
        };
        use std::io::Read as _;
        let addr = format!("127.0.0.1:{port}");
        let Ok(addr) = addr.parse::<std::net::SocketAddr>() else {
            return false;
        };
        let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1))
        else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        if stream
            .write_all(
                b"GET /health-split HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            )
            .is_err()
        {
            return false;
        }
        let mut text = String::new();
        let _ = stream.read_to_string(&mut text);
        text.starts_with("HTTP/1.1 200") && text.ends_with("ok")
    })();
    *p.delivery.lock().unwrap() = Some((ok, Instant::now()));
    ok
}

/// Retune the live proxy from the app config (Python's
/// get_range_cache_proxy retune-on-call).
pub fn retune_from_config(cfg: &crate::config::Config) {
    let p = proxy();
    let new_cfg = RangeCacheConfig::from_app_config(cfg);
    *p.cfg.lock().unwrap() = new_cfg;
}

/// A diagnostic URL with credentials, queries and signed
/// paths removed (Python `core/utils.py`
/// `redact_url_for_log`): scheme + host + port survive, the
/// query becomes `?<redacted>`, googlevideo paths collapse
/// to `/<signed-media-path>`, and a capability path segment
/// (`/<token>/proxy|file|transcode`) collapses to
/// `/<capability>/<kind>`. Anything without a scheme and
/// host is `<local-or-invalid-url>`.
pub fn redact_url_for_log(url: &str) -> String {
    let parsed = match url::Url::parse(url) {
        Ok(p) => p,
        Err(_) => {
            return if url.contains("://") {
                "<unparseable-url>".to_string()
            } else {
                "<local-or-invalid-url>".to_string()
            };
        }
    };
    let Some(host) = parsed.host_str() else {
        return "<local-or-invalid-url>".to_string();
    };
    let host = host.to_lowercase();
    let host_disp = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    let mut path = if parsed.path().is_empty() {
        "/".to_string()
    } else {
        parsed.path().to_string()
    };
    if host.ends_with("googlevideo.com") {
        path = "/<signed-media-path>".to_string();
    } else {
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if segments.len() >= 2 && matches!(segments[1], "proxy" | "file" | "transcode") {
            path = format!("/<capability>/{}", segments[1]);
        }
    }
    let suffix = if parsed.query().is_some() {
        "?<redacted>"
    } else {
        ""
    };
    format!("{}://{host_disp}{port}{path}{suffix}", parsed.scheme())
}

// ---------------------------------------------------------------------
// Request handling

fn respond_bytes(
    stream: &mut std::net::TcpStream,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn handle(mut stream: std::net::TcpStream, p: &Arc<Proxy>) {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    // Read until the header block ends (requests here are
    // header-only).
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = request.lines();
    let Some(first) = lines.next() else { return };
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let range_hdr = lines
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("range") {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    match (method, path) {
        ("GET", "/health") => {
            // Headers and body in a single write (Python's
            // _send_health: a second write can stall clients on
            // some machines).
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
        }
        ("GET", "/health-split") => {
            // The opposite on purpose: headers first, body in a
            // second write, exactly like the media path.
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 2\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            let _ = stream.write_all(b"ok");
            let _ = stream.flush();
        }
        ("GET", "/media") | ("HEAD", "/media") => {
            serve_media(&mut stream, p, query, &range_hdr, method == "HEAD");
        }
        _ => respond_bytes(&mut stream, "404 Not Found", &[], b"not found"),
    }
}

fn serve_media(
    stream: &mut std::net::TcpStream,
    p: &Arc<Proxy>,
    query: &str,
    range_hdr: &str,
    head_only: bool,
) {
    let sid = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("id="))
        .unwrap_or("");
    if sid.is_empty() {
        return respond_bytes(stream, "404 Not Found", &[], b"not found");
    }
    let Some(ent) = p.entry_for(sid) else {
        return respond_bytes(stream, "404 Not Found", &[], b"not found");
    };
    ent.touch();
    ent.await_probe();
    let (total, range_supported, content_type) = {
        let st = ent.state.lock().unwrap();
        (st.total_length, st.range_supported, st.content_type.clone())
    };
    if head_only {
        // Python's do_HEAD: advertise the real total and byte
        // ranges so the player treats the source as seekable.
        let mut headers = vec![("Content-Type", content_type)];
        if let Some(total) = total {
            headers.push(("Content-Length", total.to_string()));
        }
        headers.push(("Accept-Ranges", "bytes".to_string()));
        let mut head = String::from("HTTP/1.1 200 OK\r\n");
        for (k, v) in &headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("Connection: close\r\n\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.flush();
        return;
    }
    // No range support or unknown total: stream straight
    // through with the origin's own headers, preserving the
    // caller's Range — clamping to the inline window without a
    // known total would make the player adopt the window as
    // the entire file.
    if range_supported == Some(false) || total.is_none() {
        return serve_passthrough(stream, &ent, range_hdr);
    }
    let total = total.unwrap();
    let is_range_req = !range_hdr.is_empty();
    let (start, mut end) = if is_range_req {
        match parse_range_header(range_hdr, Some(total)) {
            Some(pair) => pair,
            None => {
                return respond_bytes(
                    stream,
                    "416 Range Not Satisfiable",
                    &[("Content-Range", format!("bytes */{total}"))],
                    b"",
                );
            }
        }
    } else {
        (0, Some(total - 1))
    };
    if start >= total {
        return respond_bytes(
            stream,
            "416 Range Not Satisfiable",
            &[("Content-Range", format!("bytes */{total}"))],
            b"",
        );
    }
    let open_ended = end.is_none();
    let cfg = p.cfg.lock().unwrap().clone();
    if open_ended {
        end = Some((start + cfg.inline_window_bytes - 1).min(total - 1));
    } else if let Some(e) = end {
        end = Some(e.min(total - 1));
    }
    let end = end.unwrap();
    if end < start {
        return respond_bytes(
            stream,
            "416 Range Not Satisfiable",
            &[("Content-Range", format!("bytes */{total}"))],
            b"",
        );
    }
    {
        let mut st = ent.state.lock().unwrap();
        st.last_req_start = start;
    }
    let length = end - start + 1;
    if is_range_req {
        let head = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: {content_type}\r\nAccept-Ranges: bytes\r\nContent-Length: {length}\r\nContent-Range: bytes {start}-{end}/{total}\r\nConnection: close\r\n\r\n"
        );
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
    } else {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
        );
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
    }
    let _ = stream.flush();
    ent.maybe_start_background_download();
    // Serve from cache where possible; stream misses from the
    // origin while caching them (Python's serve loop).
    let mut cur = start;
    while cur <= end {
        if let Some((_, seg_end)) = ent.best_segment_covering(cur) {
            let part_end = seg_end.min(end);
            match ent.stream_cached_range_to(cur, part_end, stream) {
                Ok(_) => {
                    cur = part_end + 1;
                    continue;
                }
                Err(_) => {
                    ent.load_existing_segments();
                }
            }
        }
        let nxt = ent.next_segment_start_after(cur);
        let miss_end = match nxt {
            Some(n) if n > cur => end.min(n - 1),
            _ => end,
        };
        let streamed = ent.stream_origin_range_to_and_cache(cur, miss_end, stream);
        if streamed < cur as i64 {
            break;
        }
        cur = (streamed + 1) as u64;
    }
}

/// Straight relay of the origin response (Python's
/// range-unsupported passthrough in do_GET).
fn serve_passthrough(stream: &mut std::net::TcpStream, ent: &Arc<Entry>, range_hdr: &str) {
    let mut headers = ent.origin_headers(None);
    if !range_hdr.is_empty() {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("Range"));
        headers.push(("Range".into(), range_hdr.to_string()));
    }
    match crate::http_headers::media_get(&ent.url, &ent.url, &headers) {
        Err(()) => respond_bytes(stream, "502 Bad Gateway", &[], b"Origin fetch failed"),
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let reason = if status == 206 {
                "206 Partial Content"
            } else {
                "200 OK"
            };
            let mut head = format!("HTTP/1.1 {reason}\r\n");
            let skip = [
                "transfer-encoding",
                "connection",
                "keep-alive",
                "proxy-authenticate",
                "proxy-authorization",
                "te",
                "trailers",
                "upgrade",
            ];
            for (k, v) in resp.headers().iter() {
                if skip.contains(&k.as_str().to_lowercase().as_str()) {
                    continue;
                }
                if let Ok(v) = v.to_str() {
                    head.push_str(&format!("{}: {v}\r\n", k.as_str()));
                }
            }
            head.push_str("Connection: close\r\n\r\n");
            if stream.write_all(head.as_bytes()).is_err() {
                return;
            }
            let mut reader = resp.body_mut().as_reader();
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if stream.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = stream.flush();
        }
    }
}

// ---------------------------------------------------------------------
// Player gate (Python player.py `_maybe_range_cache_url`)

fn host_of(url: &str) -> String {
    let after = url.split("://").nth(1).unwrap_or(url);
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").to_string()
    } else {
        hostport.split(':').next().unwrap_or("").to_string()
    };
    host.to_lowercase()
}

pub fn is_googlevideo_url(url: &str) -> bool {
    host_of(url).ends_with("googlevideo.com")
}

pub fn is_podcast_tracking_url(url: &str) -> bool {
    let host = host_of(url);
    !host.is_empty()
        && TRACKING_HOSTS
            .iter()
            .any(|t| host == *t || host.ends_with(&format!(".{t}")))
}

/// Decide whether playback of `url` should go through the
/// range cache, and return the URL to load. Faithful port of
/// the Python gate: non-http(s), loopback and HLS pass through;
/// googlevideo and podcast trackers always proxy; everything
/// else follows `range_cache_enabled` /
/// `range_cache_apply_all_hosts` / `range_cache_hosts`; a
/// machine that cannot deliver a local body plays direct.
pub fn maybe_range_cache_url(
    url: &str,
    headers: &[(String, String)],
    url_is_resolved: bool,
    original_url: Option<&str>,
    cfg: &crate::config::Config,
) -> String {
    retune_from_config(cfg);
    if url.is_empty() {
        return url.to_string();
    }
    // Register the ORIGINAL (pre-redirect) URL when we have
    // one, so the cache key is stable and the proxy can
    // re-follow redirects to mint a fresh signed link.
    let mut register_url = url;
    let mut skip_resolve = url_is_resolved;
    if let Some(orig) = original_url {
        let orig = orig.trim();
        if !orig.is_empty()
            && orig != url
            && (orig.starts_with("http://") || orig.starts_with("https://"))
        {
            register_url = orig;
            skip_resolve = false;
        }
    }
    let low = url.to_lowercase();
    if !(low.starts_with("http://") || low.starts_with("https://")) {
        return url.to_string();
    }
    let host = host_of(url);
    if host == "127.0.0.1" || host == "localhost" || host == "::1" {
        return url.to_string();
    }
    if low.contains(".m3u8") {
        return url.to_string();
    }
    let force = is_googlevideo_url(url) || is_podcast_tracking_url(url);
    if !force {
        if !cfg.get_bool("range_cache_enabled", true) {
            return url.to_string();
        }
        let mut apply_all = cfg.get_bool("range_cache_apply_all_hosts", true);
        let hosts: Vec<String> = cfg
            .raw()
            .get("range_cache_hosts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if hosts
            .iter()
            .any(|h| ["*", "all", "ALL"].contains(&h.trim()))
        {
            apply_all = true;
        }
        if !apply_all {
            if host.is_empty() || hosts.is_empty() {
                return url.to_string();
            }
            let host_ok = hosts.iter().any(|h| {
                let hs = h.trim().to_lowercase();
                if hs.is_empty() {
                    return false;
                }
                if let Some(suffix) = hs.strip_prefix("*.") {
                    return host.ends_with(&format!(".{suffix}")) || host == suffix;
                }
                host == hs || host.ends_with(&format!(".{hs}")) || host.contains(&hs)
            });
            if !host_ok {
                return url.to_string();
            }
        }
    }
    // Default headers merged with the caller's (the caller's
    // win), plus the promodj Referer special case.
    let mut req_headers: Vec<(String, String)> =
        vec![("User-Agent".to_string(), crate::http::user_agent())];
    for (k, v) in headers {
        if let Some(slot) = req_headers
            .iter_mut()
            .find(|(ek, _)| ek.eq_ignore_ascii_case(k))
        {
            slot.1 = v.clone();
        } else {
            req_headers.push((k.clone(), v.clone()));
        }
    }
    if host.contains("promodj.com")
        && !req_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("Referer"))
    {
        req_headers.push(("Referer".into(), "https://promodj.com/".into()));
    }
    if !can_deliver_body() {
        return url.to_string();
    }
    proxify(register_url, &req_headers, skip_resolve)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// M04 end to end, without the process-wide proxy: an Entry
    /// whose configured bootstrap (64 MiB) and chunk (32 MiB)
    /// both exceed Python's 10 MiB cap fills a 25 MiB file in
    /// the background; every range the origin sees must stay
    /// within the cap, and the cap must actually engage (the
    /// largest request is exactly 10 MiB).
    #[test]
    fn background_download_never_exceeds_ten_mib_requests() {
        let total: u64 = 25 * 1024 * 1024;
        let body: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let spans = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
        let spans_thread = spans.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let spans = spans_thread.clone();
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let Ok(n) = stream.read(&mut buf) else { return };
                    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let range = text
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.trim().eq_ignore_ascii_case("range") {
                                Some(v.trim().to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    let total = body.len() as u64;
                    let (status, start, end) = match parse_range_header(&range, Some(total)) {
                        Some((s, Some(e))) => ("206 Partial Content", s, e),
                        Some((s, None)) => ("206 Partial Content", s, total - 1),
                        None => ("200 OK", 0, total - 1),
                    };
                    spans.lock().unwrap().push((start, end));
                    let slice = &body[start as usize..=end as usize];
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        slice.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(slice);
                    let _ = stream.flush();
                });
            }
        });

        let cache_dir = std::env::temp_dir().join(format!(
            "blindrss-rc-m04-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cfg = RangeCacheConfig {
            cache_dir: cache_dir.clone(),
            prefetch_bytes: 16 * 1024 * 1024,
            inline_window_bytes: 1024 * 1024,
            initial_burst_bytes: 64 * 1024 * 1024,
            initial_inline_prefetch_bytes: 1024 * 1024,
            background_download: true,
            background_chunk_bytes: 32 * 1024 * 1024,
            debug: false,
        };
        let url = format!("http://127.0.0.1:{port}/big.mp3");
        let entry = Arc::new(Entry::new(&url, Vec::new(), cfg));
        let bg = {
            let entry = entry.clone();
            std::thread::spawn(move || entry.background_run())
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let covered = loop {
            {
                let st = entry.state.lock().unwrap();
                if st.total_length == Some(total)
                    && missing_segments(&st.segments, 0, total - 1).is_empty()
                {
                    break true;
                }
            }
            if std::time::Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        entry.stop_background();
        let _ = bg.join();
        assert!(covered, "background fill completed the file");
        let spans = spans.lock().unwrap();
        assert!(!spans.is_empty());
        for (s, e) in spans.iter() {
            // Request length is e - s + 1; the cap binds it.
            assert!(
                e - s < BG_MAX_REQUEST_BYTES,
                "origin range {s}-{e} exceeds the 10 MiB cap"
            );
        }
        let largest = spans.iter().map(|(s, e)| e - s + 1).max().unwrap();
        assert_eq!(largest, BG_MAX_REQUEST_BYTES, "the cap engaged exactly");
        let _ = std::fs::remove_dir_all(&cache_dir);
    }
}
