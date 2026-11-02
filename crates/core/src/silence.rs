//! Silence detection (Python core/audio_silence.py port).
//!
//! Two detectors, mirroring Python's two paths:
//! - ffmpeg's silencedetect filter over the whole media
//!   (`detect_silence`), ranges merged and padded the way
//!   Python's post-processing does (merge gaps up to 260 ms,
//!   shrink 60 ms of padding per side).
//! - the incremental RMS detector (`StreamingSilenceDetector` +
//!   `scan_silence_incremental`), a port of Python's
//!   `StreamingSilenceDetector` / `scan_audio_for_silence` RMS
//!   path: ffmpeg decodes to PCM, the detector consumes the
//!   stream in windows as it arrives, results are published
//!   through a progress callback while the scan is still
//!   running, the scan paces itself to stay only `lead_ms`
//!   ahead of the reported playback position (ffmpeg blocks on
//!   its stdout pipe while the scan waits, so it stops burning
//!   CPU too), and a cancelled load token kills ffmpeg instead
//!   of decoding the whole episode for a listener who moved on.
//!
//! Python's primary detector is a WebRTC VAD over decoded PCM;
//! the RMS detector is Python's own fallback when the VAD is
//! unavailable, and stays the documented substitute here (no
//! Rust VAD binding exists; see PARITY.md). The incremental
//! scan therefore runs the RMS detector with Python's RMS
//! parameters (threshold dB, minimum silence, window).

use std::io::Read;
use std::path::PathBuf;

/// Locate ffmpeg: user-configured path, explicit override env,
/// then PATH.
pub fn find_ffmpeg() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["ffmpeg.exe", "ffmpeg"]
    } else {
        &["ffmpeg"]
    };
    if let Some(p) = crate::dependency::user_tool_path("ffmpeg", names) {
        return Some(p);
    }
    if let Ok(p) = std::env::var("BLINDRSS_FFMPEG")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    crate::dependency::find_on_path(names)
}

/// Parse ffmpeg silencedetect stderr into raw (start, end)
/// ranges in seconds. An unmatched trailing start is dropped.
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

/// Merge ranges separated by at most `merge_gap` seconds, then
/// shrink each range by `padding` on both sides (Python's
/// silence_skip_merge_gap_ms / silence_skip_padding_ms). Ranges
/// that vanish under padding are dropped.
pub fn finalize_ranges(
    mut ranges: Vec<(f64, f64)>,
    merge_gap: f64,
    padding: f64,
) -> Vec<(f64, f64)> {
    ranges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start - last.1 <= merge_gap => {
                last.1 = last.1.max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    merged
        .into_iter()
        .map(|(s, e)| (s + padding, e - padding))
        .filter(|(s, e)| e > s)
        .collect()
}

/// Run ffmpeg silencedetect over a local path or an http(s) URL.
/// `threshold_db` is negative (Python default -38.0);
/// `min_silence` is the shortest silence that counts, in seconds
/// (Python default 0.7).
pub fn detect_silence(
    media: &str,
    threshold_db: f64,
    min_silence: f64,
) -> Result<Vec<(f64, f64)>, String> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| "ffmpeg not found".to_string())?;
    let filter = format!("silencedetect=noise={threshold_db}dB:d={min_silence}");
    let output = std::process::Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-i",
            media,
            "-af",
            &filter,
            "-f",
            "null",
            "-",
        ])
        .output()
        .map_err(|e| format!("ffmpeg run: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = parse_silencedetect(&stderr);
    Ok(finalize_ranges(raw, 0.26, 0.06))
}

// ---------------------------------------------------------------------
// Incremental RMS detection (Python StreamingSilenceDetector /
// detect_silence_ranges_from_pcm / scan_audio_for_silence).
// ---------------------------------------------------------------------

/// RMS of one PCM chunk (Python `_rms`): 16-bit little-endian
/// samples, multi-channel collapsed to mono by averaging.
fn rms(chunk: &[u8], sample_width: usize, channels: usize) -> f64 {
    if sample_width != 2 {
        if chunk.is_empty() {
            return 0.0;
        }
        let sq: f64 = chunk
            .iter()
            .map(|b| {
                let v = *b as f64 - 128.0;
                v * v
            })
            .sum();
        return (sq / chunk.len() as f64).sqrt();
    }
    if chunk.len() < 2 {
        return 0.0;
    }
    let channels = channels.max(1);
    let frames = chunk.len() / 2 / channels;
    if frames == 0 {
        return 0.0;
    }
    let mut sq = 0.0f64;
    for f in 0..frames {
        let mut mono = 0.0f64;
        for c in 0..channels {
            let i = (f * channels + c) * 2;
            mono += i16::from_le_bytes([chunk[i], chunk[i + 1]]) as f64;
        }
        mono /= channels as f64;
        sq += mono * mono;
    }
    (sq / frames as f64).sqrt()
}

/// Python `_dbfs`: level of an RMS value against full scale.
fn dbfs(rms: f64) -> f64 {
    if rms <= 0.0 {
        -120.0
    } else {
        20.0 * (rms / 32768.0).log10()
    }
}

/// Merge millisecond ranges that overlap or touch (Python
/// `merge_ranges`); invalid/backwards ranges are dropped.
pub fn merge_ranges_ms(mut ranges: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    ranges.retain(|(s, e)| e >= s);
    ranges.sort();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Python `merge_ranges_with_gap`: merge ranges separated by
/// at most `gap_ms` (smooths tiny non-silent blips between long
/// quiet stretches).
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

/// The player's publish transform (Python player `_publish`):
/// expand each range by `pad_ms` on both sides (starts clamp to
/// 0), then merge ranges separated by at most `merge_gap_ms`.
pub fn publish_ranges_ms(ranges: &[(i64, i64)], pad_ms: i64, merge_gap_ms: i64) -> Vec<(i64, i64)> {
    let padded: Vec<(i64, i64)> = ranges
        .iter()
        .map(|(s, e)| ((s - pad_ms).max(0), e + pad_ms))
        .collect();
    merge_ranges_with_gap_ms(padded, merge_gap_ms)
}

/// Streaming RMS silence detector (Python
/// `StreamingSilenceDetector`): feed decoded PCM in any chunk
/// sizes; silent spans are recorded window by window as the
/// audio arrives, so early results are available through
/// `snapshot` long before the source ends.
pub struct StreamingSilenceDetector {
    sample_width: usize,
    channels: usize,
    window_ms: i64,
    threshold_db: f64,
    min_silence_windows: i64,
    window_bytes: usize,
    buf: Vec<u8>,
    current_window: i64,
    silent_run: i64,
    run_start_window: Option<i64>,
    ranges: Vec<(i64, i64)>,
}

impl StreamingSilenceDetector {
    pub fn new(
        sample_rate: u32,
        sample_width: usize,
        channels: usize,
        window_ms: i64,
        min_silence_ms: i64,
        threshold_db: f64,
    ) -> Self {
        let window_ms = window_ms.max(10);
        Self {
            sample_width,
            channels: channels.max(1),
            window_ms,
            threshold_db,
            min_silence_windows: (min_silence_ms + window_ms - 1) / window_ms.max(1),
            window_bytes: ((sample_rate as i64
                * window_ms
                * sample_width as i64
                * channels.max(1) as i64)
                / 1000)
                .max(1) as usize,
            buf: Vec::new(),
            current_window: 0,
            silent_run: 0,
            run_start_window: None,
            ranges: Vec::new(),
        }
    }

    /// Python's defaults: 16 kHz mono 16-bit, 30 ms windows,
    /// 800 ms minimum silence at -40 dB.
    pub fn defaults() -> Self {
        Self::new(16_000, 2, 1, 30, 800, -40.0)
    }

    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.buf.extend_from_slice(data);
        while self.buf.len() >= self.window_bytes {
            let window: Vec<u8> = self.buf.drain(..self.window_bytes).collect();
            let silent = dbfs(rms(&window, self.sample_width, self.channels)) <= self.threshold_db;
            if silent {
                if self.run_start_window.is_none() {
                    self.run_start_window = Some(self.current_window);
                }
                self.silent_run += 1;
            } else {
                self.close_run();
            }
            self.current_window += 1;
        }
    }

    fn close_run(&mut self) {
        let Some(start_window) = self.run_start_window.take() else {
            self.silent_run = 0;
            return;
        };
        if self.silent_run >= self.min_silence_windows.max(1) {
            self.ranges.push((
                start_window * self.window_ms,
                self.current_window * self.window_ms,
            ));
        }
        self.silent_run = 0;
    }

    /// Ranges closed so far, merged with `gap_ms` — what the
    /// paced scan hands to its progress callback (Python passes
    /// the same merged-so-far set out of its detector loop).
    pub fn snapshot(&self, gap_ms: i64) -> Vec<(i64, i64)> {
        merge_ranges_with_gap_ms(self.ranges.clone(), gap_ms)
    }

    /// Python `finalize`: a still-open silent run closes at the
    /// last full window; a trailing partial window is not
    /// analysed (Python's flush feeds an empty chunk, which its
    /// feed() ignores). Returns the merged ranges.
    pub fn finalize(mut self) -> Vec<(i64, i64)> {
        if self.run_start_window.is_some() {
            self.close_run();
        }
        merge_ranges_ms(self.ranges)
    }
}

/// Python `detect_silence_ranges_from_pcm`: run the streaming
/// detector over an in-memory PCM chunk sequence.
pub fn detect_silence_ranges_from_pcm(
    chunks: &[&[u8]],
    sample_rate: u32,
    sample_width: usize,
    channels: usize,
    window_ms: i64,
    min_silence_ms: i64,
    threshold_db: f64,
) -> Vec<(i64, i64)> {
    let mut det = StreamingSilenceDetector::new(
        sample_rate,
        sample_width,
        channels,
        window_ms,
        min_silence_ms,
        threshold_db,
    );
    for chunk in chunks {
        det.feed(chunk);
    }
    det.finalize()
}

/// Knobs for `scan_silence_incremental` (Python
/// `scan_audio_for_silence` parameters as the player passes
/// them, with the player's remote-stream adjustments already
/// applied by the caller through `for_remote`).
#[derive(Clone, Debug)]
pub struct SilenceScanOptions {
    pub sample_rate: u32,
    pub window_ms: i64,
    pub min_silence_ms: i64,
    pub threshold_db: f64,
    pub merge_gap_ms: i64,
    pub threads: Option<u32>,
    /// Stay at most this far ahead of the playback position;
    /// 0 disables pacing (Python: `lead_ms` must be positive
    /// for pacing to engage).
    pub lead_ms: i64,
    /// Progress callback cadence in scanned milliseconds
    /// (Python's VAD loop publishes every 5 s of audio).
    pub progress_interval_ms: i64,
}

impl Default for SilenceScanOptions {
    fn default() -> Self {
        // Python config defaults (core/config.py).
        Self {
            sample_rate: 16_000,
            window_ms: 25,
            min_silence_ms: 700,
            threshold_db: -38.0,
            merge_gap_ms: 260,
            threads: None,
            lead_ms: 300_000,
            progress_interval_ms: 5_000,
        }
    }
}

impl SilenceScanOptions {
    /// Python's player adjustments for remote streams: the
    /// 8 kHz scan rate, a threshold no higher than -52 dB, a
    /// minimum silence of at least 1200 ms, a merge gap of at
    /// most 200 ms, and one ffmpeg thread (local files get the
    /// 16 kHz rate and two threads).
    pub fn for_remote(mut self, remote: bool) -> Self {
        if remote {
            self.sample_rate = 8_000;
            if self.threshold_db > -52.0 {
                self.threshold_db = -52.0;
            }
            if self.min_silence_ms < 1200 {
                self.min_silence_ms = 1200;
            }
            if self.merge_gap_ms > 200 {
                self.merge_gap_ms = 200;
            }
            self.threads = Some(1);
        } else {
            self.threads = Some(2);
        }
        self
    }
}

/// True for an http(s) source that is not the local proxy
/// (Python's `is_remote` test in the silence worker).
pub fn is_remote_source(source: &str) -> bool {
    (source.starts_with("http://") || source.starts_with("https://"))
        && !source.contains("127.0.0.1")
        && !source.contains("localhost")
}

/// Incremental silence scan (Python `scan_audio_for_silence`,
/// RMS path): ffmpeg decodes `source` to 16-bit PCM and the
/// streaming detector consumes it as it arrives.
///
/// - `cancel`: when the load token is cancelled or superseded,
///   ffmpeg is killed and the scan returns
///   `Err("silence scan cancelled")`; ranges already published
///   through `on_progress` stay published (the caller keeps
///   them, as Python's worker does).
/// - `position_ms`: the shared playback-position snapshot the
///   UI thread refreshes (a lock-free atomic, never a media
///   control — see the player accessibility contract). With a
///   positive `lead_ms` the scan waits instead of reading once
///   it is that far ahead, which leaves ffmpeg blocked on its
///   own stdout pipe.
/// - `on_progress`: called with the merged ranges so far
///   roughly every `progress_interval_ms` of scanned audio.
///
/// HTTP headers follow Python: a `User-Agent` entry becomes
/// ffmpeg's `-user_agent`, the rest go in `-headers`; with no
/// headers the shared app identity is sent (core `http`
/// mandate — Python's hard-coded fallback UA is not copied).
pub fn scan_silence_incremental(
    source: &str,
    ffmpeg: &std::path::Path,
    options: &SilenceScanOptions,
    headers: &[(String, String)],
    cancel: &crate::playback::LoadToken,
    position_ms: Option<&std::sync::atomic::AtomicI64>,
    on_progress: &dyn Fn(Vec<(i64, i64)>),
) -> Result<Vec<(i64, i64)>, String> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    if let Some(threads) = options.threads.filter(|t| *t > 0) {
        cmd.args(["-threads", &threads.to_string()]);
    }
    let is_http = source.starts_with("http://") || source.starts_with("https://");
    if is_http {
        let mut header_str = String::new();
        let mut user_agent = String::new();
        for (k, v) in headers {
            if k.eq_ignore_ascii_case("user-agent") {
                user_agent = v.clone();
            } else {
                header_str.push_str(&format!("{k}: {v}\r\n"));
            }
        }
        if user_agent.is_empty() {
            user_agent = crate::http::user_agent();
        }
        cmd.args(["-user_agent", &user_agent]);
        if !header_str.is_empty() {
            cmd.args(["-headers", &header_str]);
        }
    }
    cmd.args([
        "-i",
        source,
        "-ac",
        "1",
        "-ar",
        &options.sample_rate.to_string(),
        "-f",
        "s16le",
        "-",
    ]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg spawn: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg stdout missing")?;

    let mut detector = StreamingSilenceDetector::new(
        options.sample_rate,
        2,
        1,
        options.window_ms,
        options.min_silence_ms,
        options.threshold_db,
    );
    let bytes_per_ms = (options.sample_rate as f64 * 2.0) / 1000.0;
    let paced = position_ms.is_some() && options.lead_ms > 0;
    let mut read_bytes: u64 = 0;
    let mut last_progress_ms: i64 = 0;
    let mut buf = [0u8; 65536];
    let result = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            break Err("silence scan cancelled".to_string());
        }
        if paced {
            // Stay `lead_ms` ahead of the listener and no
            // further. While we hold off reading, ffmpeg blocks
            // writing to the pipe, so it stops burning CPU too.
            let scanned_ms = (read_bytes as f64 / bytes_per_ms) as i64;
            let pos = position_ms
                .map(|p| p.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(0);
            if scanned_ms - pos > options.lead_ms {
                // Python waits on the abort event in 0.5 s
                // slices; 100 ms slices keep cancellation
                // prompt without busy-waiting.
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        }
        match stdout.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                read_bytes += n as u64;
                detector.feed(&buf[..n]);
                let scanned_ms = (read_bytes as f64 / bytes_per_ms) as i64;
                if scanned_ms - last_progress_ms >= options.progress_interval_ms {
                    last_progress_ms = scanned_ms;
                    on_progress(detector.snapshot(options.merge_gap_ms));
                }
            }
            Err(e) => {
                let _ = child.kill();
                break Err(format!("ffmpeg read: {e}"));
            }
        }
    };
    let status = child.wait();
    if let Err(e) = &result {
        return Err(e.clone());
    }
    match status {
        Ok(s) if s.success() => Ok(detector.finalize()),
        Ok(s) => Err(format!(
            "ffmpeg exited with code {}",
            s.code().unwrap_or(-1)
        )),
        Err(e) => Err(format!("ffmpeg wait: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    /// 16 kHz mono: `tone_ms` of loud samples then `quiet_ms`
    /// of digital silence, in whole 10 ms windows.
    fn tone_then_silence(tone_ms: usize, quiet_ms: usize) -> Vec<u8> {
        let per_ms = 16;
        let mut samples = vec![10_000i16; tone_ms * per_ms];
        samples.extend(vec![0i16; quiet_ms * per_ms]);
        pcm(&samples)
    }

    #[test]
    fn rms_and_dbfs_match_python_formulas() {
        // Full-scale square wave averages to full scale.
        assert!((rms(&pcm(&[32767, -32768]), 2, 1) - 32767.5).abs() < 0.01);
        assert_eq!(dbfs(0.0), -120.0);
        assert!((dbfs(32768.0)).abs() < 0.001);
        assert!((dbfs(3276.8) + 20.0).abs() < 0.001);
        // Stereo collapses by averaging the pair, like Python.
        assert!((rms(&pcm(&[10_000, -10_000]), 2, 2)).abs() < 0.01);
        assert!((rms(&pcm(&[10_000, 10_000]), 2, 2) - 10_000.0).abs() < 0.01);
    }

    #[test]
    fn detector_finds_silence_after_tone() {
        let data = tone_then_silence(500, 1500);
        let ranges = detect_silence_ranges_from_pcm(&[&data], 16_000, 2, 1, 10, 800, -40.0);
        assert_eq!(ranges, vec![(500, 2000)]);
    }

    #[test]
    fn detector_ignores_short_gaps_and_quiet_tone() {
        // 300 ms gap is below the 800 ms minimum.
        let mut data = tone_then_silence(500, 300);
        data.extend(tone_then_silence(500, 0));
        assert!(detect_silence_ranges_from_pcm(&[&data], 16_000, 2, 1, 10, 800, -40.0).is_empty());
        // A quiet-but-audible tone (-20 dBFS) is not silence at -40 dB.
        let quiet_tone = pcm(&vec![3276i16; 16 * 2000]);
        assert!(
            detect_silence_ranges_from_pcm(&[&quiet_tone], 16_000, 2, 1, 10, 800, -40.0).is_empty()
        );
    }

    #[test]
    fn chunked_feeding_matches_whole_buffer() {
        let data = tone_then_silence(500, 1500);
        let whole = detect_silence_ranges_from_pcm(&[&data], 16_000, 2, 1, 30, 800, -40.0);
        let mut det = StreamingSilenceDetector::new(16_000, 2, 1, 30, 800, -40.0);
        // Odd chunk sizes split windows and samples mid-frame.
        for chunk in data.chunks(7_919) {
            det.feed(chunk);
        }
        assert_eq!(det.finalize(), whole);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].0, 510); // first fully-silent 30 ms window
    }

    #[test]
    fn snapshot_publishes_closed_ranges_before_finalize() {
        let mut det = StreamingSilenceDetector::new(16_000, 2, 1, 10, 800, -40.0);
        det.feed(&tone_then_silence(500, 1500));
        // The run is still open (no non-silent window yet), so
        // nothing is published — Python publishes closed ranges.
        assert!(det.snapshot(200).is_empty());
        det.feed(&tone_then_silence(100, 0));
        assert_eq!(det.snapshot(200), vec![(500, 2000)]);
    }

    #[test]
    fn merge_and_publish_follow_python() {
        assert_eq!(
            merge_ranges_with_gap_ms(vec![(0, 100), (250, 300), (900, 1000)], 200),
            vec![(0, 300), (900, 1000)]
        );
        assert_eq!(merge_ranges_ms(vec![(10, 5), (0, 10)]), vec![(0, 10)]);
        // Publish expands by the pad and merges across the gap.
        assert_eq!(
            publish_ranges_ms(&[(100, 1000), (1200, 2000)], 60, 260),
            vec![(40, 2060)]
        );
        assert_eq!(publish_ranges_ms(&[(30, 100)], 60, 260), vec![(0, 160)]);
    }

    #[test]
    fn remote_options_apply_python_adjustments() {
        let local = SilenceScanOptions::default().for_remote(false);
        assert_eq!(local.sample_rate, 16_000);
        assert_eq!(local.threads, Some(2));
        let remote = SilenceScanOptions::default().for_remote(true);
        assert_eq!(remote.sample_rate, 8_000);
        assert_eq!(remote.threshold_db, -52.0);
        assert_eq!(remote.min_silence_ms, 1200);
        assert_eq!(remote.merge_gap_ms, 200);
        assert_eq!(remote.threads, Some(1));
        assert!(is_remote_source("https://cdn.example.com/ep.mp3"));
        assert!(!is_remote_source("http://127.0.0.1:9/media?id=1"));
        assert!(!is_remote_source("/tmp/ep.mp3"));
    }
}
