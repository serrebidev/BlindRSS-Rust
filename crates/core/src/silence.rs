//! Silence detection (Python core/audio_silence.py port, RMS
//! path): ffmpeg's silencedetect filter over the media, ranges
//! merged and padded the way Python's post-processing does
//! (merge gaps up to 260 ms, shrink 60 ms of padding per side).
//!
//! Python's primary detector is a WebRTC VAD over decoded PCM;
//! ffmpeg's threshold detector is the honest equivalent available
//! without a native VAD binding, and matches Python's own RMS
//! fallback parameters (threshold dB, minimum silence).

use std::path::PathBuf;

/// Locate ffmpeg through the shared resolution policy (Audit 8
/// Task 23, O14): user-configured path, explicit override env,
/// the managed install, PATH, bundled and known package
/// locations — see `dependency::find_tool`.
pub fn find_ffmpeg() -> Option<PathBuf> {
    crate::dependency::find_tool("ffmpeg")
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

/// Python's config defaults for the two finalize tunables
/// (`silence_skip_merge_gap_ms` / `silence_skip_padding_ms`,
/// core/config.py): 260 ms merge gap, 60 ms padding.
pub const DEFAULT_MERGE_GAP_S: f64 = 0.26;
pub const DEFAULT_PADDING_S: f64 = 0.06;

/// Run ffmpeg silencedetect over a local path or an http(s) URL,
/// with Python's default merge/padding. `threshold_db` is
/// negative (Python default -38.0); `min_silence` is the shortest
/// silence that counts, in seconds (Python default 0.7).
pub fn detect_silence(
    media: &str,
    threshold_db: f64,
    min_silence: f64,
) -> Result<Vec<(f64, f64)>, String> {
    detect_silence_tuned(
        media,
        threshold_db,
        min_silence,
        DEFAULT_MERGE_GAP_S,
        DEFAULT_PADDING_S,
    )
}

/// `detect_silence` with caller-supplied finalize tunables (the
/// `silence_skip_merge_gap_ms` / `silence_skip_padding_ms` config
/// keys, in seconds). Audit 7: the values used to be hardcoded,
/// so tuning saved by Python's settings was silently ignored.
pub fn detect_silence_tuned(
    media: &str,
    threshold_db: f64,
    min_silence: f64,
    merge_gap: f64,
    padding: f64,
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
    Ok(finalize_ranges(raw, merge_gap, padding))
}
