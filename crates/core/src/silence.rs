//! Silence detection and the Skip Silence policy (Python
//! `core/audio_silence.py` + the silence regions of
//! `gui/player.py`, ported by Audit 8 Task 21).
//!
//! Detection mirrors Python's two paths over ffmpeg-decoded
//! PCM (mono s16le): the primary speech-aware detector is the
//! WebRTC VAD (via the `webrtc-vad` crate, which wraps
//! libfvad — the same WebRTC VAD C code Python drives through
//! `webrtcvad`) ANDed with Python's per-frame RMS/dBFS check;
//! when the VAD is unavailable for the scan rate, Python's
//! RMS `StreamingSilenceDetector` is the fallback — the same
//! fallback Python itself takes when `webrtcvad` is missing.
//! The old ffmpeg `silencedetect` runner is gone: it blocked
//! on the whole source, took no headers and could not be
//! cancelled. The scan below streams PCM incrementally, is
//! paced behind the playback position, publishes progress on
//! Python's 5 s-of-audio cadence, and kills + reaps its
//! ffmpeg child on every exit path.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Locate ffmpeg through the shared resolution policy (Audit 8
/// Task 23, O14): user-configured path, explicit override env,
/// the managed install, PATH, bundled and known package
/// locations — see `dependency::find_tool`.
pub fn find_ffmpeg() -> Option<PathBuf> {
    crate::dependency::find_tool("ffmpeg")
}

/// Parse ffmpeg silencedetect stderr into raw (start, end)
/// ranges in seconds. An unmatched trailing start is dropped.
/// Retained from the pre-Task-21 detector; the silencedetect
/// output shape is also what the batch-3 fixtures pin.
pub fn parse_silencedetect(stderr: &str) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut open_start: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(idx) = line.find("silence_start:") {
            open_start = line[idx + "silence_start:".len()..]
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok());
        } else if let Some(idx) = line.find("silence_end:") {
            let end = line[idx + "silence_end:".len()..]
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok());
            if let (Some(start), Some(end)) = (open_start.take(), end)
                && end > start
            {
                out.push((start, end));
            }
        }
    }
    out
}

/// Python `merge_ranges`: drop inverted ranges, sort, merge
/// ranges that overlap or touch (next.start <= cur.end + 1).
pub fn merge_ranges_ms(mut ranges: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    ranges.retain(|(s, e)| e >= s);
    ranges.sort_by_key(|(s, e)| (*s, *e));
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Python `merge_ranges_with_gap`: `merge_ranges`, then also
/// merge ranges separated by at most `gap_ms`.
pub fn merge_ranges_with_gap_ms(ranges: Vec<(i64, i64)>, gap_ms: i64) -> Vec<(i64, i64)> {
    let merged = merge_ranges_ms(ranges);
    let gap = gap_ms.max(0);
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in merged {
        match out.last_mut() {
            Some(last) if s <= last.1 + gap => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Python's publish transform (`gui/player.py` `_publish` in
/// the silence worker): EXPAND every range by `pad_ms` on
/// both sides (start clamped at 0), then merge across gaps up
/// to `merge_gap_ms`. Padding expands in Python; the pre-Task
/// 21 Rust finalize shrank ranges instead (Audit 8 M09).
pub fn pad_and_merge_ms(
    ranges: Vec<(i64, i64)>,
    pad_ms: i64,
    merge_gap_ms: i64,
) -> Vec<(i64, i64)> {
    let padded: Vec<(i64, i64)> = ranges
        .into_iter()
        .map(|(s, e)| ((s - pad_ms).max(0), e + pad_ms))
        .collect();
    merge_ranges_with_gap_ms(padded, merge_gap_ms)
}

/// Post-process raw ranges in seconds with Python's publish
/// arithmetic: pad (EXPAND) by `padding`, then merge across
/// gaps up to `merge_gap`.
pub fn finalize_ranges(ranges: Vec<(f64, f64)>, merge_gap: f64, padding: f64) -> Vec<(f64, f64)> {
    let ms: Vec<(i64, i64)> = ranges
        .iter()
        .map(|(s, e)| ((s * 1000.0).round() as i64, (e * 1000.0).round() as i64))
        .collect();
    pad_and_merge_ms(
        ms,
        (padding * 1000.0).round() as i64,
        (merge_gap * 1000.0).round() as i64,
    )
    .into_iter()
    .map(|(s, e)| (s as f64 / 1000.0, e as f64 / 1000.0))
    .collect()
}

/// Python's config defaults for the two publish tunables
/// (`silence_skip_merge_gap_ms` / `silence_skip_padding_ms`,
/// core/config.py): 260 ms merge gap, 60 ms padding.
pub const DEFAULT_MERGE_GAP_S: f64 = 0.26;
pub const DEFAULT_PADDING_S: f64 = 0.06;

/// RMS level of a mono i16 frame in dBFS (Python `_rms` +
/// `_dbfs`): silence (or an empty frame) is -120 dB.
pub fn rms_dbfs(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return -120.0;
    }
    let sq: f64 = samples.iter().map(|v| (*v as f64) * (*v as f64)).sum();
    let rms = (sq / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        -120.0
    } else {
        20.0 * (rms / 32768.0).log10()
    }
}

fn bytes_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// Python `StreamingSilenceDetector`: windowed RMS detection
/// over a PCM byte stream. Windows are consumed whole; a
/// trailing partial window is dropped, and a run still open
/// at `finish` closes there — both exactly as Python's
/// `finalize` behaves (its buffer flush is a no-op).
pub struct PcmSilenceDetector {
    window_ms: i64,
    window_bytes: usize,
    min_windows: i64,
    threshold_db: f64,
    buf: Vec<u8>,
    current_window: i64,
    silent_run: i64,
    run_start: Option<i64>,
    ranges: Vec<(i64, i64)>,
}

impl PcmSilenceDetector {
    pub fn new(sample_rate: i64, window_ms: i64, min_silence_ms: i64, threshold_db: f64) -> Self {
        let window_ms = window_ms.max(10);
        PcmSilenceDetector {
            window_ms,
            window_bytes: (sample_rate * window_ms * 2 / 1000).max(2) as usize,
            min_windows: (min_silence_ms + window_ms - 1) / window_ms.max(1),
            threshold_db,
            buf: Vec::new(),
            current_window: 0,
            silent_run: 0,
            run_start: None,
            ranges: Vec::new(),
        }
        .with_min_windows_floor()
    }

    fn with_min_windows_floor(mut self) -> Self {
        // Python: max(1, ceil(min_silence_ms / window_ms)).
        if self.min_windows < 1 {
            self.min_windows = 1;
        }
        self
    }

    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.buf.extend_from_slice(data);
        let mut off = 0usize;
        while self.buf.len() - off >= self.window_bytes {
            let window = &self.buf[off..off + self.window_bytes];
            let samples = bytes_to_samples(window);
            let silent = rms_dbfs(&samples) <= self.threshold_db;
            if silent {
                if self.run_start.is_none() {
                    self.run_start = Some(self.current_window);
                }
                self.silent_run += 1;
            } else {
                self.close_run();
            }
            self.current_window += 1;
            off += self.window_bytes;
        }
        self.buf.drain(..off);
    }

    fn close_run(&mut self) {
        if let Some(start) = self.run_start.take()
            && self.silent_run >= self.min_windows
        {
            self.ranges
                .push((start * self.window_ms, self.current_window * self.window_ms));
        }
        self.silent_run = 0;
    }

    /// Closed silence runs so far (unmerged), for progress.
    pub fn ranges_so_far(&self) -> Vec<(i64, i64)> {
        self.ranges.clone()
    }

    /// Audio consumed so far, in milliseconds.
    pub fn scanned_ms(&self) -> i64 {
        self.current_window * self.window_ms
    }

    /// Python `finalize`: close any open run and merge.
    pub fn finish(mut self) -> Vec<(i64, i64)> {
        if self.run_start.is_some() {
            self.close_run();
        }
        merge_ranges_ms(std::mem::take(&mut self.ranges))
    }
}

/// Python `_detect_vad_ranges` as a streaming detector: each
/// frame (10/20/30 ms) is silent when the WebRTC VAD says
/// "not speech" AND the frame's RMS level is at or below the
/// threshold. Frame sizes outside 10/20/30 coerce to 30 and
/// aggressiveness clamps to 0..=3, as in Python.
pub struct VadSilenceDetector {
    vad: Option<webrtc_vad::Vad>,
    frame_ms: i64,
    frame_bytes: usize,
    aggressiveness: i64,
    min_silence_ms: i64,
    merge_gap_ms: i64,
    threshold_db: f64,
    buf: Vec<u8>,
    offset_ms: i64,
    silence_start: Option<i64>,
    ranges: Vec<(i64, i64)>,
}

impl VadSilenceDetector {
    /// The WebRTC VAD accepts 8000/16000/32000/48000 Hz.
    pub fn available(sample_rate: i64) -> bool {
        matches!(sample_rate, 8000 | 16000 | 32000 | 48000)
    }

    pub fn new(
        sample_rate: i64,
        frame_ms: i64,
        min_silence_ms: i64,
        aggressiveness: i64,
        merge_gap_ms: i64,
        threshold_db: f64,
    ) -> Self {
        let frame_ms = if matches!(frame_ms, 10 | 20 | 30) {
            frame_ms
        } else {
            30
        };
        let aggressiveness = aggressiveness.clamp(0, 3);
        let rate = match sample_rate {
            8000 => Some(webrtc_vad::SampleRate::Rate8kHz),
            16000 => Some(webrtc_vad::SampleRate::Rate16kHz),
            32000 => Some(webrtc_vad::SampleRate::Rate32kHz),
            48000 => Some(webrtc_vad::SampleRate::Rate48kHz),
            _ => None,
        };
        let mode = match aggressiveness {
            0 => webrtc_vad::VadMode::Quality,
            1 => webrtc_vad::VadMode::LowBitrate,
            2 => webrtc_vad::VadMode::Aggressive,
            _ => webrtc_vad::VadMode::VeryAggressive,
        };
        VadSilenceDetector {
            vad: rate.map(|r| webrtc_vad::Vad::new_with_rate_and_mode(r, mode)),
            frame_ms,
            frame_bytes: (sample_rate * frame_ms * 2 / 1000).max(2) as usize,
            aggressiveness,
            min_silence_ms,
            merge_gap_ms,
            threshold_db,
            buf: Vec::new(),
            offset_ms: 0,
            silence_start: None,
            ranges: Vec::new(),
        }
    }

    pub fn frame_ms(&self) -> i64 {
        self.frame_ms
    }

    pub fn aggressiveness(&self) -> i64 {
        self.aggressiveness
    }

    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.buf.extend_from_slice(data);
        let mut off = 0usize;
        while self.buf.len() - off >= self.frame_bytes {
            let frame = &self.buf[off..off + self.frame_bytes];
            let samples = bytes_to_samples(frame);
            // A VAD error (impossible for exact frames) counts
            // as speech: never skip on a detector failure.
            let speech = self
                .vad
                .as_mut()
                .map(|vad| vad.is_voice_segment(&samples).unwrap_or(true))
                .unwrap_or(true);
            let silent = !speech && rms_dbfs(&samples) <= self.threshold_db;
            if silent {
                if self.silence_start.is_none() {
                    self.silence_start = Some(self.offset_ms);
                }
            } else if let Some(start) = self.silence_start.take()
                && self.offset_ms - start >= self.min_silence_ms
            {
                self.ranges.push((start, self.offset_ms));
            }
            self.offset_ms += self.frame_ms;
            off += self.frame_bytes;
        }
        self.buf.drain(..off);
    }

    /// Closed silence runs so far (unmerged), for progress.
    pub fn ranges_so_far(&self) -> Vec<(i64, i64)> {
        self.ranges.clone()
    }

    /// Audio consumed so far, in milliseconds.
    pub fn scanned_ms(&self) -> i64 {
        self.offset_ms
    }

    /// Close any open run and merge with the gap, as Python's
    /// `_detect_vad_ranges` returns.
    pub fn finish(mut self) -> Vec<(i64, i64)> {
        if let Some(start) = self.silence_start.take()
            && self.offset_ms - start >= self.min_silence_ms
        {
            self.ranges.push((start, self.offset_ms));
        }
        merge_ranges_with_gap_ms(std::mem::take(&mut self.ranges), self.merge_gap_ms)
    }
}

enum Detector {
    Rms(PcmSilenceDetector),
    Vad(VadSilenceDetector),
}

impl Detector {
    fn feed(&mut self, data: &[u8]) {
        match self {
            Detector::Rms(d) => d.feed(data),
            Detector::Vad(d) => d.feed(data),
        }
    }

    fn ranges_so_far(&self) -> Vec<(i64, i64)> {
        match self {
            Detector::Rms(d) => d.ranges_so_far(),
            Detector::Vad(d) => d.ranges_so_far(),
        }
    }

    fn scanned_ms(&self) -> i64 {
        match self {
            Detector::Rms(d) => d.scanned_ms(),
            Detector::Vad(d) => d.scanned_ms(),
        }
    }

    fn finish(self) -> Vec<(i64, i64)> {
        match self {
            Detector::Rms(d) => d.finish(),
            Detector::Vad(d) => d.finish(),
        }
    }
}

/// Scan settings, mirroring the values Python's player worker
/// reads from config (with the remote adjustments already
/// applied by the caller).
#[derive(Clone, Debug)]
pub struct SilenceScanConfig {
    /// Decode sample rate: 16000 local / 8000 remote in Python.
    pub sample_rate: i64,
    /// RMS fallback window (Python `silence_skip_window_ms`).
    pub window_ms: i64,
    /// Shortest silence that counts (`silence_skip_min_ms`).
    pub min_silence_ms: i64,
    /// RMS threshold (`silence_skip_threshold_db`).
    pub threshold_db: f64,
    /// Detector merge gap (`silence_skip_merge_gap_ms`).
    pub merge_gap_ms: i64,
    /// WebRTC VAD aggressiveness 0..=3.
    pub vad_aggressiveness: i64,
    /// WebRTC VAD frame size: 10, 20 or 30 ms.
    pub vad_frame_ms: i64,
    /// Python's `detection_mode == "vad"` request; the RMS
    /// fallback engages when the VAD cannot serve the rate.
    pub use_vad: bool,
    /// Request headers for http(s) sources (Python passes the
    /// playback headers dict to ffmpeg).
    pub headers: Vec<(String, String)>,
    /// User-Agent when `headers` carries none (the app UA from
    /// core's user_agents policy; Python hardcodes a browser
    /// UA here, which the Rust port must not — AGENTS.md
    /// mandate 1).
    pub default_user_agent: String,
    /// ffmpeg `-threads` (Python `silence_scan_threads`).
    pub threads: Option<i64>,
    /// Below-normal priority where the platform supports it
    /// (Python `silence_scan_low_priority`).
    pub low_priority: bool,
    /// Pacing lead in ms (Python `silence_scan_lead_seconds`):
    /// the scan reads no further than this ahead of the
    /// position the provider reports. 0 disables pacing.
    pub lead_ms: i64,
}

/// The player's scan + skip settings, resolved from config
/// exactly as Python's silence worker resolves them:
/// `core/config.py` DEFAULT_CONFIG supplies the effective
/// defaults (Python's config loader merges them into every
/// config), and remote sources get Python's conservative
/// adjustments (VAD aggressiveness 0, threshold capped at
/// -52 dB, minimum silence at least 1200 ms, merge gap at
/// most 200 ms, 8000 Hz decode, 1 thread).
#[derive(Clone, Debug)]
pub struct SilenceScanSettings {
    pub scan: SilenceScanConfig,
    /// `silence_skip_padding_ms` — applied by the publisher
    /// (the player), as in Python.
    pub padding_ms: i64,
    /// `silence_scan_delay_s` — the scan starts this long
    /// after the load, only while playing.
    pub scan_delay_s: f64,
    /// `silence_skip_resume_backoff_ms` (the policy caps it
    /// at 1000 ms for remote media).
    pub resume_backoff_ms: i64,
    /// `silence_skip_retrigger_backoff_ms`.
    pub retrigger_backoff_ms: i64,
}

impl SilenceScanSettings {
    pub fn from_config(cfg: &crate::config::Config, is_remote: bool) -> Self {
        let mut vad_aggressiveness = cfg.get_i64("silence_vad_aggressiveness", 2);
        let mut threshold_db = cfg
            .raw()
            .get("silence_skip_threshold_db")
            .and_then(|v| v.as_f64())
            .unwrap_or(-38.0);
        let mut min_silence_ms = cfg.get_i64("silence_skip_min_ms", 700);
        let mut merge_gap_ms = cfg.get_i64("silence_skip_merge_gap_ms", 260);
        if is_remote {
            if vad_aggressiveness > 0 {
                vad_aggressiveness = 0;
            }
            threshold_db = threshold_db.min(-52.0);
            min_silence_ms = min_silence_ms.max(1200);
            merge_gap_ms = merge_gap_ms.min(200);
        }
        let sample_rate = if is_remote {
            cfg.get_i64("silence_scan_remote_sample_rate", 8000)
        } else {
            cfg.get_i64("silence_scan_sample_rate", 16000)
        };
        let threads = cfg.get_i64("silence_scan_threads", if is_remote { 1 } else { 2 });
        SilenceScanSettings {
            scan: SilenceScanConfig {
                sample_rate,
                window_ms: cfg.get_i64("silence_skip_window_ms", 25),
                min_silence_ms,
                threshold_db,
                merge_gap_ms,
                vad_aggressiveness,
                vad_frame_ms: cfg.get_i64("silence_vad_frame_ms", 30),
                use_vad: true,
                headers: Vec::new(),
                default_user_agent: String::new(),
                threads: (threads > 0).then_some(threads),
                low_priority: cfg.get_bool("silence_scan_low_priority", true),
                lead_ms: cfg.get_i64("silence_scan_lead_seconds", 300) * 1000,
            },
            padding_ms: cfg.get_i64("silence_skip_padding_ms", 60),
            scan_delay_s: cfg
                .raw()
                .get("silence_scan_delay_s")
                .and_then(|v| v.as_f64())
                .unwrap_or(4.0),
            resume_backoff_ms: cfg.get_i64("silence_skip_resume_backoff_ms", 360),
            retrigger_backoff_ms: cfg.get_i64("silence_skip_retrigger_backoff_ms", 1400),
        }
    }
}

/// Progress sink for `scan_silence`: (ranges closed so far,
/// milliseconds of audio scanned) — return false to abort
/// the scan as stale.
pub type ScanProgress<'a> = dyn FnMut(&[(i64, i64)], i64) -> bool + 'a;

#[derive(Debug)]
pub enum ScanError {
    /// The abort flag was set (or a progress consumer marked
    /// the scan stale): the child was killed and reaped.
    Cancelled,
    /// ffmpeg could not be started.
    Spawn(String),
    /// ffmpeg ran but failed, or its output could not be read.
    Ffmpeg(String),
}

/// Build the ffmpeg argument list (Python `scan_audio_for_
/// silence`): header options only for http(s) sources — a
/// `User-Agent` header becomes `-user_agent`, the rest one
/// `-headers` block; with no headers at all, the default UA
/// is passed, as Python does.
fn ffmpeg_args(source: &str, cfg: &SilenceScanConfig) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
    ];
    if let Some(t) = cfg.threads
        && t > 0
    {
        args.push("-threads".into());
        args.push(t.to_string());
    }
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        if cfg.headers.is_empty() {
            if !cfg.default_user_agent.is_empty() {
                args.push("-user_agent".into());
                args.push(cfg.default_user_agent.clone());
            }
        } else {
            let mut header_block = String::new();
            for (k, v) in &cfg.headers {
                if k.eq_ignore_ascii_case("user-agent") {
                    args.push("-user_agent".into());
                    args.push(v.clone());
                } else {
                    header_block.push_str(&format!("{k}: {v}\r\n"));
                }
            }
            if !header_block.is_empty() {
                args.push("-headers".into());
                args.push(header_block);
            }
        }
    }
    args.push("-i".into());
    args.push(source.to_string());
    args.push("-ac".into());
    args.push("1".into());
    args.push("-ar".into());
    args.push(cfg.sample_rate.to_string());
    args.push("-f".into());
    args.push("s16le".into());
    args.push("-".into());
    args
}

/// Spawn ffmpeg with the owned-subprocess discipline of
/// `dependency::spawn_tool` (a just-written executable can
/// fail with ETXTBSY briefly): retry that one error with
/// backoff inside a 2 s budget; any other error is immediate.
fn spawn_ffmpeg(
    ffmpeg: &Path,
    args: &[String],
    low_priority: bool,
) -> Result<std::process::Child, ScanError> {
    #[cfg(unix)]
    const ETXTBSY: i32 = 26;
    let started = Instant::now();
    let mut backoff = Duration::from_millis(10);
    loop {
        let mut cmd = std::process::Command::new(ffmpeg);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // CREATE_NO_WINDOW, plus BELOW_NORMAL_PRIORITY_CLASS
            // when low priority was requested (Python's flags).
            let mut flags: u32 = 0x0800_0000;
            if low_priority {
                flags |= 0x0000_4000;
            }
            cmd.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = low_priority;
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) => {
                #[cfg(unix)]
                let busy = e.raw_os_error() == Some(ETXTBSY);
                #[cfg(not(unix))]
                let busy = false;
                if !busy || started.elapsed() + backoff > Duration::from_secs(2) {
                    return Err(ScanError::Spawn(format!(
                        "{} failed to start: {e}",
                        ffmpeg.display()
                    )));
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(100));
            }
        }
    }
}

fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Wait up to `budget` for the child to exit, killing it past
/// that (Python's `proc.wait(timeout=5)` then kill).
fn wait_bounded(
    child: &mut std::process::Child,
    budget: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if started.elapsed() >= budget {
                    kill_and_reap(child);
                    return child.wait();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Sleep in slices while a paced scan waits for the listener
/// to catch up; returns true when the abort flag fired.
fn sleep_or_abort(abort: &AtomicBool, total: Duration) -> bool {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total {
        if abort.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(step.min(total - slept));
        slept += step;
    }
    abort.load(Ordering::Relaxed)
}

/// Scan `source` for silence (Python `scan_audio_for_
/// silence`): ffmpeg decodes to mono s16le PCM which streams
/// through the VAD (or RMS fallback) detector.
///
/// * `abort` cancels: the child is killed and reaped and the
///   result is `ScanError::Cancelled` — no ffmpeg survives.
/// * Pacing: with `lead_ms > 0`, reading pauses while the
///   scanned position is more than the lead ahead of
///   `position()` (ffmpeg blocks on its own stdout pipe).
/// * `progress` fires every 5000 ms of scanned audio with
///   the gap-merged ranges closed so far; returning false
///   marks the scan stale and aborts it (Python's publish
///   gate). Python delivers progress only on its VAD path;
///   delivering it on the RMS fallback too is a deliberate
///   superset (the plan requires incremental results).
/// * The returned ranges are the detector's final set
///   (VAD: gap-merged; RMS: merged — as Python returns them);
///   padding is the caller's publish step (`pad_and_merge_ms`).
pub fn scan_silence(
    ffmpeg: &Path,
    source: &str,
    cfg: &SilenceScanConfig,
    abort: &AtomicBool,
    position: &dyn Fn() -> Option<i64>,
    progress: &mut ScanProgress<'_>,
) -> Result<Vec<(i64, i64)>, ScanError> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    let mut detector = if cfg.use_vad && VadSilenceDetector::available(cfg.sample_rate) {
        Detector::Vad(VadSilenceDetector::new(
            cfg.sample_rate,
            cfg.vad_frame_ms,
            cfg.min_silence_ms,
            cfg.vad_aggressiveness,
            cfg.merge_gap_ms,
            cfg.threshold_db,
        ))
    } else {
        Detector::Rms(PcmSilenceDetector::new(
            cfg.sample_rate,
            cfg.window_ms,
            cfg.min_silence_ms,
            cfg.threshold_db,
        ))
    };
    let args = ffmpeg_args(source, cfg);
    let mut child = spawn_ffmpeg(ffmpeg, &args, cfg.low_priority)?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    // Drain stderr concurrently into a shared buffer (capped):
    // Python drains it after the loop, which can stall a
    // chatty ffmpeg on a full pipe; the content is used the
    // same way (the error message on a nonzero exit). The
    // drain thread is never joined on an error path: a killed
    // child's own children can inherit the pipe and hold it
    // open, and cancellation must not wait on them.
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let stderr_thread = child.stderr.take().map(|mut err| {
        let buf = stderr_buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match err.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut data = buf.lock().unwrap();
                        if data.len() < 64 * 1024 {
                            data.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        })
    });

    let bytes_per_ms = (cfg.sample_rate * 2) as f64 / 1000.0;
    let paced = cfg.lead_ms > 0;
    let mut read_bytes: i64 = 0;
    let mut last_progress_ms: i64 = 0;
    let mut buf = [0u8; 65536];
    let outcome = loop {
        if abort.load(Ordering::Relaxed) {
            kill_and_reap(&mut child);
            break Err(ScanError::Cancelled);
        }
        if paced {
            let scanned = read_bytes as f64 / bytes_per_ms;
            if let Some(pos) = position()
                && scanned - pos as f64 > cfg.lead_ms as f64
            {
                // Hold: ffmpeg blocks writing to the pipe, so
                // it stops burning CPU too (Python's pacing).
                if sleep_or_abort(abort, Duration::from_millis(500)) {
                    kill_and_reap(&mut child);
                    break Err(ScanError::Cancelled);
                }
                continue;
            }
        }
        match stdout.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                read_bytes += n as i64;
                detector.feed(&buf[..n]);
                let scanned = detector.scanned_ms();
                if scanned - last_progress_ms >= 5000 {
                    last_progress_ms = scanned;
                    let so_far =
                        merge_ranges_with_gap_ms(detector.ranges_so_far(), cfg.merge_gap_ms);
                    if !progress(&so_far, scanned) {
                        kill_and_reap(&mut child);
                        break Err(ScanError::Cancelled);
                    }
                }
            }
            Err(e) => {
                kill_and_reap(&mut child);
                break Err(ScanError::Ffmpeg(format!("ffmpeg output read: {e}")));
            }
        }
    };
    if let Err(e) = outcome {
        // The drain thread is left detached on error paths
        // (see above): it exits when the last pipe writer does.
        drop(stderr_thread);
        return Err(e);
    }
    let ranges = detector.finish();
    let status = wait_bounded(&mut child, Duration::from_secs(5))
        .map_err(|e| ScanError::Ffmpeg(format!("ffmpeg wait: {e}")))?;
    // The child has exited, so its stderr reaches EOF as soon
    // as the drain copies the tail; give it a bounded moment
    // rather than joining unconditionally.
    if let Some(h) = stderr_thread {
        for _ in 0..50 {
            if h.is_finished() {
                let _ = h.join();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let stderr = stderr_buf.lock().unwrap().clone();
    if abort.load(Ordering::Relaxed) {
        return Err(ScanError::Cancelled);
    }
    if !status.success() {
        // Python raises with ffmpeg's stderr; the source URL
        // ffmpeg echoes into it is redacted first (diagnostic
        // hygiene: signed URLs carry credentials in queries).
        let text = String::from_utf8_lossy(&stderr).trim().to_string();
        let tail: String = text
            .chars()
            .rev()
            .take(500)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let tail = tail.replace(source, &crate::rangecache::redact_url_for_log(source));
        return Err(ScanError::Ffmpeg(format!(
            "ffmpeg exited with code {}: {tail}",
            status.code().unwrap_or(-1)
        )));
    }
    Ok(ranges)
}

// ---------------------------------------------------------------------
// Skip policy (Python gui/player.py `_maybe_skip_silence`, the
// verify block in its timer, `_note_user_seek`'s silence half
// and the floor ratchet in its position processing).
// ---------------------------------------------------------------------

/// Per-tick inputs to the skip decision that live outside the
/// policy: backend/session facts and the two backoff tunables.
#[derive(Clone, Copy, Debug)]
pub struct SkipContext {
    /// A scan has published (Python `_silence_scan_ready`).
    pub ready: bool,
    /// Backend seekable report: `Some(false)` blocks, `None`
    /// (unknown) proceeds — Python's rule.
    pub seekable: Option<bool>,
    /// A resume restore is pending or in flight.
    pub resume_pending: bool,
    /// The seek slider is being dragged.
    pub dragging: bool,
    /// Playback is cast to a remote device (Python's player
    /// hosts casting; the Rust player passes false — casting
    /// is a main-window surface there).
    pub casting: bool,
    /// The current media is remote (http, non-loopback):
    /// selects the conservative cooldowns/windows and caps
    /// the resume backoff at 1000 ms.
    pub is_remote: bool,
    /// `silence_skip_resume_backoff_ms`.
    pub resume_backoff_ms: i64,
    /// `silence_skip_retrigger_backoff_ms`.
    pub retrigger_backoff_ms: i64,
}

#[derive(Clone, Copy, Debug)]
struct VerifyState {
    until: Instant,
    target_ms: i64,
    #[allow(dead_code)]
    source_ms: i64,
    attempted: bool,
}

/// The guarded silence-skip state machine. Clock-injected:
/// callers pass `Instant`s, so the whole policy is testable
/// without a window or a backend. One instance per player
/// session; `reset` on every new load (Python's
/// `_cancel_silence_scan` state clearing).
#[derive(Debug)]
pub struct SilenceSkipPolicy {
    floor_ms: i64,
    reset_floor: bool,
    allow_backwards_until: Option<Instant>,
    recent_seek: Option<(i64, Instant)>,
    pause_until: Option<Instant>,
    last_skip_at: Option<Instant>,
    last_target_ms: Option<i64>,
    last_seek_at: Option<Instant>,
    last_idx: Option<usize>,
    active_target: Option<i64>,
    verify: Option<VerifyState>,
}

impl Default for SilenceSkipPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl SilenceSkipPolicy {
    pub fn new() -> Self {
        SilenceSkipPolicy {
            floor_ms: 0,
            reset_floor: false,
            allow_backwards_until: None,
            recent_seek: None,
            pause_until: None,
            last_skip_at: None,
            last_target_ms: None,
            last_seek_at: None,
            last_idx: None,
            active_target: None,
            verify: None,
        }
    }

    /// New load / scan cancel: all skip state clears (Python
    /// `_cancel_silence_scan`).
    pub fn reset(&mut self) {
        *self = SilenceSkipPolicy::new();
    }

    /// Python `_note_user_seek`'s silence half: skipping
    /// pauses (1.2 s local, 2.5 s remote), the floor resets
    /// to the landing position on the next tick, and any
    /// pending landing verification is cancelled.
    pub fn note_user_seek(&mut self, now: Instant, is_remote: bool) {
        self.reset_floor = true;
        self.pause_until = Some(now + Duration::from_millis(if is_remote { 2500 } else { 1200 }));
        self.verify = None;
    }

    /// Record an app-issued seek (resume restore, silence
    /// skip, verify re-seek) for the floor ratchet: a target
    /// landing more than 1200 ms behind the position opens
    /// Python's 3 s allow-backwards window, during which the
    /// floor follows the position down instead of ratcheting.
    pub fn note_app_seek(&mut self, target_ms: i64, pos_ms: i64, now: Instant) {
        self.recent_seek = Some((target_ms, now));
        if target_ms + 1200 < pos_ms {
            self.allow_backwards_until = Some(now + Duration::from_secs(3));
        }
    }

    /// The floor ratchet (Python's position processing): the
    /// floor is the furthest position playback has
    /// legitimately reached — the max of itself, a recent
    /// seek target (8 s window) and the current position —
    /// except right after a user seek (reset to position) or
    /// inside an allow-backwards window (follows position).
    pub fn update_floor(&mut self, pos_ms: i64, now: Instant) {
        if self.reset_floor {
            self.floor_ms = pos_ms;
            self.reset_floor = false;
        } else if self.allow_backwards_until.is_some_and(|u| now < u) {
            self.floor_ms = pos_ms;
        } else {
            let mut candidate = self.floor_ms;
            if let Some((target, ts)) = self.recent_seek
                && now.duration_since(ts) < Duration::from_secs(8)
            {
                candidate = candidate.max(target);
            }
            self.floor_ms = candidate.max(pos_ms);
        }
    }

    /// Landing verification (Python's timer verify block):
    /// after a skip, if the position is still short of the
    /// target past the threshold (700 ms local / 1200 ms
    /// remote) and the minimum wait since the skip seek has
    /// passed (0.35 s / 0.5 s), re-seek ONCE. Landing inside
    /// the threshold, window expiry (3 s / 5 s), or a user
    /// seek ends verification. Returns the re-seek target.
    pub fn verify_tick(&mut self, pos_ms: i64, now: Instant, is_remote: bool) -> Option<i64> {
        let verify = self.verify?;
        if now > verify.until {
            self.verify = None;
            return None;
        }
        let threshold = if is_remote { 1200 } else { 700 };
        if verify.target_ms - pos_ms <= threshold {
            self.verify = None;
            return None;
        }
        if !verify.attempted {
            let min_wait = Duration::from_millis(if is_remote { 500 } else { 350 });
            let waited = self
                .last_seek_at
                .is_none_or(|ts| now.duration_since(ts) >= min_wait);
            if waited {
                if let Some(v) = self.verify.as_mut() {
                    v.attempted = true;
                }
                return Some(verify.target_ms);
            }
        }
        None
    }

    /// One skip evaluation (Python `_maybe_skip_silence`).
    /// Returns the position to seek to when a skip fires; the
    /// caller performs the seek directly (silence skips do
    /// not enter the user-seek guard — Audit 8 Task 19).
    pub fn evaluate(
        &mut self,
        ranges: &[(i64, i64)],
        pos_ms: i64,
        now: Instant,
        ctx: &SkipContext,
    ) -> Option<i64> {
        if ctx.casting {
            return None;
        }
        if ctx.seekable == Some(false) {
            return None;
        }
        if ctx.resume_pending {
            return None;
        }
        if !ctx.ready || ranges.is_empty() {
            return None;
        }
        if self.pause_until.is_some_and(|u| now < u) {
            return None;
        }
        if ctx.dragging {
            return None;
        }
        if pos_ms + 500 < self.floor_ms {
            return None;
        }
        let cooldown = Duration::from_millis(if ctx.is_remote { 5000 } else { 2500 });
        if self
            .last_skip_at
            .is_some_and(|ts| now.duration_since(ts) < cooldown)
        {
            return None;
        }
        let mut resume_backoff = ctx.resume_backoff_ms.max(0);
        if ctx.is_remote && resume_backoff > 1000 {
            resume_backoff = 1000;
        }
        let retrigger = ctx.retrigger_backoff_ms;
        let current_target = self.active_target;
        for (idx, &(start, end)) in ranges.iter().enumerate() {
            if pos_ms < start - 1000 {
                // Ranges are sorted; no later range can match.
                break;
            }
            if start - 100 <= pos_ms && pos_ms <= end - 100 {
                let target_ms = end + resume_backoff;
                if target_ms < self.floor_ms {
                    return None;
                }
                // Loop guard: never re-seek to (nearly) the
                // target of the last skip.
                if self
                    .last_target_ms
                    .is_some_and(|lt| (lt - target_ms).abs() <= 1000)
                {
                    return None;
                }
                self.active_target = Some(target_ms);
                self.last_skip_at = Some(now);
                self.last_idx = Some(idx);
                self.last_target_ms = Some(target_ms);
                self.last_seek_at = Some(now);
                self.verify = Some(VerifyState {
                    until: now + Duration::from_millis(if ctx.is_remote { 5000 } else { 3000 }),
                    target_ms,
                    source_ms: pos_ms,
                    attempted: false,
                });
                return Some(target_ms);
            }
        }
        // Post-loop bookkeeping (Python's tail block): retire
        // the active target once playback passes it, and the
        // last-range memory once playback is well beyond it.
        if current_target.is_some_and(|t| pos_ms > t + 500) {
            self.active_target = None;
        }
        if let Some(last_idx) = self.last_idx
            && let Some(&(_, last_end)) = ranges.get(last_idx)
            && pos_ms > last_end + retrigger + 300
        {
            self.last_idx = None;
        }
        if let Some(lt) = self.last_target_ms
            && self
                .last_seek_at
                .is_some_and(|ts| now.duration_since(ts) > Duration::from_secs(2))
            && (pos_ms - lt).abs() > retrigger
        {
            self.last_target_ms = None;
        }
        None
    }

    pub fn floor_ms(&self) -> i64 {
        self.floor_ms
    }

    pub fn last_idx(&self) -> Option<usize> {
        self.last_idx
    }

    pub fn verify_active(&self) -> bool {
        self.verify.is_some()
    }
}
