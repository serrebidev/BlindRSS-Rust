//! Gap 7 playback residuals: incremental silence scan over a
//! real ffmpeg decode (Python scan_audio_for_silence RMS path),
//! its pacing and cancellation. Skips when ffmpeg is absent.

use blindrss_core::playback::LoadGate;
use blindrss_core::silence::{SilenceScanOptions, scan_silence_incremental};
use std::sync::atomic::{AtomicI64, Ordering};

fn write_wav(path: &std::path::Path, tone_ms: usize, quiet_ms: usize) {
    let rate = 16_000u32;
    let mut samples: Vec<i16> = Vec::new();
    for i in 0..tone_ms * 16 {
        // 440 Hz tone at half scale.
        let t = i as f64 / rate as f64;
        samples.push((16_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16);
    }
    samples.extend(vec![0i16; quiet_ms * 16]);
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for s in &samples {
        out.extend(s.to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}

fn temp_wav(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blindrss-residuals-{}-{}",
        std::process::id(),
        name
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("tone.wav")
}

fn options() -> SilenceScanOptions {
    SilenceScanOptions {
        sample_rate: 16_000,
        window_ms: 10,
        min_silence_ms: 800,
        threshold_db: -40.0,
        merge_gap_ms: 200,
        threads: None,
        lead_ms: 0,
        progress_interval_ms: 500,
    }
}

#[test]
fn incremental_scan_finds_the_silent_tail_and_publishes_progress() {
    let Some(ffmpeg) = blindrss_core::silence::find_ffmpeg() else {
        return;
    };
    let path = temp_wav("scan");
    write_wav(&path, 500, 1500);
    let gate = LoadGate::new();
    let token = gate.begin();
    let published = std::sync::Mutex::new(Vec::new());
    let progress = |ranges: Vec<(i64, i64)>| {
        published.lock().unwrap().push(ranges);
    };
    let ranges = scan_silence_incremental(
        path.to_str().unwrap(),
        &ffmpeg,
        &options(),
        &[],
        &token,
        None,
        &progress,
    )
    .unwrap();
    assert_eq!(ranges, vec![(500, 2000)]);
    // Progress fired at least once while scanning (the file is
    // decoded in one or two chunks; every publish is the merged
    // set so far, never a partial-window guess).
    assert!(!published.lock().unwrap().is_empty());
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn incremental_scan_honours_cancellation() {
    let Some(ffmpeg) = blindrss_core::silence::find_ffmpeg() else {
        return;
    };
    let path = temp_wav("cancel");
    write_wav(&path, 500, 1500);
    let gate = LoadGate::new();
    let token = gate.begin();
    gate.cancel_current();
    let started = std::time::Instant::now();
    let err = scan_silence_incremental(
        path.to_str().unwrap(),
        &ffmpeg,
        &options(),
        &[],
        &token,
        None,
        &|_| {},
    )
    .unwrap_err();
    assert_eq!(err, "silence scan cancelled");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn incremental_scan_paces_behind_the_playback_position() {
    let Some(ffmpeg) = blindrss_core::silence::find_ffmpeg() else {
        return;
    };
    let path = temp_wav("paced");
    write_wav(&path, 500, 1500);
    let gate = LoadGate::new();
    let token = gate.begin();
    // Listener is parked at 0 with a 1 ms lead: after the
    // first chunk the scan must wait instead of finishing,
    // until the load is cancelled.
    let position = AtomicI64::new(0);
    let gate2 = gate.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        gate2.cancel_current();
    });
    let mut opts = options();
    opts.lead_ms = 1;
    let started = std::time::Instant::now();
    let err = scan_silence_incremental(
        path.to_str().unwrap(),
        &ffmpeg,
        &opts,
        &[],
        &token,
        Some(&position),
        &|_| {},
    )
    .unwrap_err();
    assert_eq!(err, "silence scan cancelled");
    // It waited for the cancel instead of racing to the end.
    assert!(started.elapsed() >= std::time::Duration::from_millis(350));
    assert_eq!(position.load(Ordering::Relaxed), 0);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
