//! The media-player equalizer (port of Python `core/equalizer.py`
//! plus the DSP the Rust playback paths apply themselves).
//!
//! Config shape (config.json key "equalizer"), identical to
//! Python's so a shared config keeps its settings:
//! {enabled, preamp, bands[10], preset}. Amps clamp to libVLC's
//! [-20, +20] dB. Preamp is stored in libVLC units where 12 dB
//! is unity (VLC's filter scales the signal by 0.25 twice and
//! the preamp compensates), so the DSP applies (preamp - 12).
//!
//! Built-in presets are libVLC's own table (what Python reads
//! through libvlc_audio_equalizer_new_from_preset), with the
//! preamp already converted out of the +12 convention.
//!
//! The DSP is a cascade of RBJ peaking biquads at the ISO band
//! frequencies — the same shape as Luna EQ / LADSPA 10-band
//! equalizers (that project is GPL + PipeWire-only, so this is
//! an independent implementation, not a port of its code). It
//! approximates libVLC's filter bank rather than replicating
//! its exact IIR coefficients.

use crate::config::Config;

pub const BAND_COUNT: usize = 10;
pub const AMP_MIN: f64 = -20.0;
pub const AMP_MAX: f64 = 20.0;
pub const BAND_FREQUENCIES: [f64; BAND_COUNT] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Human label for a band center ("500 Hz", "16 kHz").
pub fn format_band_label(freq: f64) -> String {
    if freq >= 1000.0 {
        format!("{} kHz", freq / 1000.0)
    } else {
        format!("{} Hz", freq)
    }
}

/// Clamp one amp value the way Python's clamp_amp does
/// (non-numbers and NaN become 0, result rounded to 0.1).
pub fn clamp_amp(value: f64) -> f64 {
    if value.is_nan() {
        return 0.0;
    }
    (value.clamp(AMP_MIN, AMP_MAX) * 10.0).round() / 10.0
}

#[derive(Debug, Clone, PartialEq)]
pub struct EqConfig {
    pub enabled: bool,
    /// Stored (libVLC) units; 12.0 is unity.
    pub preamp: f64,
    pub bands: [f64; BAND_COUNT],
    pub preset: Option<String>,
}

impl Default for EqConfig {
    fn default() -> Self {
        EqConfig {
            enabled: false,
            preamp: 0.0,
            bands: [0.0; BAND_COUNT],
            preset: None,
        }
    }
}

impl EqConfig {
    /// Coerce arbitrary stored JSON into a well-formed config
    /// (Python's normalize_config).
    pub fn from_value(v: Option<&serde_json::Value>) -> EqConfig {
        let Some(v) = v else {
            return EqConfig::default();
        };
        let mut bands = [0.0; BAND_COUNT];
        if let Some(arr) = v.get("bands").and_then(|b| b.as_array()) {
            for (i, slot) in bands.iter_mut().enumerate() {
                if let Some(n) = arr.get(i).and_then(|x| x.as_f64()) {
                    *slot = clamp_amp(n);
                }
            }
        }
        EqConfig {
            enabled: v.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false),
            preamp: v
                .get("preamp")
                .and_then(|p| p.as_f64())
                .map(clamp_amp)
                .unwrap_or(0.0),
            bands,
            preset: v
                .get("preset")
                .and_then(|p| p.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }
    }

    pub fn from_config(cfg: &Config) -> EqConfig {
        EqConfig::from_value(cfg.raw().get("equalizer"))
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "preamp": self.preamp,
            "bands": self.bands.to_vec(),
            "preset": self.preset,
        })
    }

    pub fn is_flat(&self) -> bool {
        self.preamp == 0.0 && self.bands.iter().all(|b| *b == 0.0)
    }

    /// The gain the DSP actually applies for the preamp:
    /// libVLC units minus the 12 dB convention offset.
    pub fn effective_preamp_db(&self) -> f64 {
        self.preamp - 12.0
    }
}

/// libVLC's built-in presets: (display name, effective preamp
/// dB, band gains). Values from VLC's equalizer_presets.h via
/// the same read path Python uses; preamp converted (stored
/// minus 12) as documented at the top of this module.
pub const PRESETS: [(&str, f64, [f64; BAND_COUNT]); 18] = [
    ("Flat", 0.0, [0.0; BAND_COUNT]),
    (
        "Classical",
        0.0,
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -7.2, -7.2, -7.2, -9.6],
    ),
    (
        "Club",
        -6.0,
        [0.0, 0.0, 8.0, 5.6, 5.6, 5.6, 3.2, 0.0, 0.0, 0.0],
    ),
    (
        "Dance",
        -7.0,
        [9.6, 7.2, 2.4, 0.0, 0.0, -5.6, -7.2, -7.2, 0.0, 0.0],
    ),
    (
        "Full bass",
        -7.0,
        [-8.0, 9.6, 9.6, 5.6, 1.6, -4.0, -8.0, -10.4, -11.2, -11.2],
    ),
    (
        "Full bass and treble",
        -8.0,
        [7.2, 5.6, 0.0, -7.2, -4.8, 1.6, 8.0, 11.2, 12.0, 12.0],
    ),
    (
        "Full treble",
        -9.0,
        [-9.6, -9.6, -9.6, -4.0, 2.4, 11.2, 16.0, 16.0, 16.0, 16.8],
    ),
    (
        "Headphones",
        -8.0,
        [4.8, 11.2, 5.6, -3.2, -2.4, 1.6, 4.8, 9.6, 12.8, 14.4],
    ),
    (
        "Large Hall",
        -7.0,
        [10.4, 10.4, 5.6, 5.6, 0.0, -4.8, -4.8, -4.8, 0.0, 0.0],
    ),
    (
        "Live",
        -5.0,
        [-4.8, 0.0, 4.0, 5.6, 5.6, 5.6, 4.0, 2.4, 2.4, 2.4],
    ),
    (
        "Party",
        -6.0,
        [7.2, 7.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.2, 7.2],
    ),
    (
        "Pop",
        -6.0,
        [-1.6, 4.8, 7.2, 8.0, 5.6, 0.0, -2.4, -2.4, -1.6, -1.6],
    ),
    (
        "Reggae",
        -4.0,
        [0.0, 0.0, 0.0, -5.6, 0.0, 6.4, 6.4, 0.0, 0.0, 0.0],
    ),
    (
        "Rock",
        -7.0,
        [8.0, 4.8, -5.6, -8.0, -3.2, 4.0, 8.8, 11.2, 11.2, 11.2],
    ),
    (
        "Ska",
        -6.0,
        [-2.4, -4.8, -4.0, 0.0, 4.0, 5.6, 8.8, 9.6, 11.2, 9.6],
    ),
    (
        "Soft",
        -7.0,
        [4.8, 1.6, 0.0, -2.4, 0.0, 4.0, 8.0, 9.6, 11.2, 12.0],
    ),
    (
        "Soft rock",
        -5.0,
        [4.0, 4.0, 2.4, 0.0, -4.0, -5.6, -3.2, 0.0, 2.4, 8.8],
    ),
    (
        "Techno",
        -7.0,
        [8.0, 5.6, 0.0, -5.6, -4.8, 0.0, 8.0, 9.6, 9.6, 8.8],
    ),
];

#[derive(Debug, Clone, PartialEq)]
pub struct UserPreset {
    pub name: String,
    pub preamp: f64,
    pub bands: [f64; BAND_COUNT],
}

/// Normalize the stored user-preset list (Python's
/// normalize_user_presets): de-duplicated by name, clamped.
pub fn normalize_user_presets(raw: Option<&serde_json::Value>) -> Vec<UserPreset> {
    let mut out: Vec<UserPreset> = Vec::new();
    let Some(arr) = raw.and_then(|v| v.as_array()) else {
        return out;
    };
    for item in arr {
        let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let name = name.trim().to_string();
        if name.is_empty()
            || out
                .iter()
                .any(|p: &UserPreset| p.name.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let c = EqConfig::from_value(Some(item));
        out.push(UserPreset {
            name,
            preamp: c.preamp,
            bands: c.bands,
        });
    }
    out
}

/// The preset list with `name` added or overwritten (Python's
/// upsert_user_preset).
pub fn upsert_user_preset(
    raw: Option<&serde_json::Value>,
    name: &str,
    preamp: f64,
    bands: [f64; BAND_COUNT],
) -> Vec<serde_json::Value> {
    let mut presets: Vec<UserPreset> = normalize_user_presets(raw)
        .into_iter()
        .filter(|p| !p.name.eq_ignore_ascii_case(name.trim()))
        .collect();
    if !name.trim().is_empty() {
        let c = EqConfig {
            enabled: true,
            preamp,
            bands,
            preset: None,
        };
        presets.push(UserPreset {
            name: name.trim().to_string(),
            preamp: clamp_amp(c.preamp),
            bands: c.bands.map(clamp_amp),
        });
    }
    presets
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "preamp": p.preamp,
                "bands": p.bands.to_vec(),
            })
        })
        .collect()
}

/// One peaking biquad (RBJ audio EQ cookbook), Direct Form I
/// with per-channel state.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: [f64; 2],
    x2: [f64; 2],
    y1: [f64; 2],
    y2: [f64; 2],
}

impl Biquad {
    fn peaking(sample_rate: f64, freq: f64, gain_db: f64, q: f64) -> Biquad {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        let a0 = 1.0 + alpha / a;
        Biquad {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / a) / a0,
            x1: [0.0; 2],
            x2: [0.0; 2],
            y1: [0.0; 2],
            y2: [0.0; 2],
        }
    }

    #[inline]
    fn process(&mut self, ch: usize, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1[ch] + self.b2 * self.x2[ch]
            - self.a1 * self.y1[ch]
            - self.a2 * self.y2[ch];
        self.x2[ch] = self.x1[ch];
        self.x1[ch] = x;
        self.y2[ch] = self.y1[ch];
        self.y1[ch] = y;
        y
    }
}

/// Stateful EQ over interleaved stereo i16 PCM.
pub struct EqProcessor {
    filters: Vec<Biquad>,
    preamp_gain: f64,
}

impl EqProcessor {
    /// Q = sqrt(2)≈1.41 gives roughly octave-wide bells at these
    /// ISO centers (adjacent bands one octave apart).
    pub fn new(sample_rate: f64, cfg: &EqConfig) -> EqProcessor {
        let filters = BAND_FREQUENCIES
            .iter()
            .zip(cfg.bands.iter())
            .filter(|(_, g)| **g != 0.0)
            .map(|(f, g)| Biquad::peaking(sample_rate, *f, *g, std::f64::consts::SQRT_2))
            .collect();
        EqProcessor {
            filters,
            preamp_gain: 10f64.powf(cfg.effective_preamp_db() / 20.0),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.filters.is_empty() && (self.preamp_gain - 1.0).abs() < 1e-9
    }

    /// Process interleaved stereo samples in place.
    pub fn process_i16(&mut self, samples: &mut [i16]) {
        if self.is_identity() {
            return;
        }
        for frame in samples.as_chunks_mut::<2>().0 {
            for (ch, slot) in frame.iter_mut().enumerate() {
                let mut x = *slot as f64 * self.preamp_gain;
                for f in self.filters.iter_mut() {
                    x = f.process(ch, x);
                }
                *slot = x.clamp(-32768.0, 32767.0) as i16;
            }
        }
    }
}

/// The ffmpeg `-af` chain reproducing this EQ for the stream
/// paths (proxy/player): volume for the preamp, one `equalizer`
/// filter per non-zero band (width one octave, matching the
/// biquad Q above).
pub fn ffmpeg_filter_chain(cfg: &EqConfig) -> String {
    let mut parts: Vec<String> = Vec::new();
    let pre = cfg.effective_preamp_db();
    if pre != 0.0 {
        parts.push(format!("volume={pre:.1}dB"));
    }
    for (freq, gain) in BAND_FREQUENCIES.iter().zip(cfg.bands.iter()) {
        if *gain != 0.0 {
            parts.push(format!(
                "equalizer=f={freq}:width_type=o:width=1:g={gain:.1}"
            ));
        }
    }
    parts.join(",")
}
