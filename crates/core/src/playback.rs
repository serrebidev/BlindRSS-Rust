//! Playback resume policy and Python-compatible state identifiers.

use crate::{Db, PlaybackState, config::Config};

pub fn playback_id(article_id: &str) -> String {
    format!("article:{article_id}")
}

pub fn saved_state(
    db: &Db,
    article_id: &str,
    url: &str,
) -> rusqlite::Result<Option<PlaybackState>> {
    for id in [
        playback_id(article_id),
        article_id.to_string(),
        url.to_string(),
    ] {
        if !id.is_empty()
            && let Some(state) = db.playback_state(&id)?
        {
            return Ok(Some(state));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy)]
pub struct ResumePolicy {
    pub enabled: bool,
    pub min_ms: i64,
    pub back_ms: i64,
    pub complete_threshold_ms: i64,
    pub save_interval: std::time::Duration,
}

impl ResumePolicy {
    pub fn from_config(cfg: &Config) -> Self {
        let interval = cfg
            .raw()
            .get("resume_save_interval_s")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(15.0);
        Self {
            enabled: cfg.get_bool("resume_playback", true),
            min_ms: cfg.get_i64("resume_min_ms", 0).max(0),
            back_ms: cfg.get_i64("resume_back_ms", 10_000).max(0),
            complete_threshold_ms: cfg.get_i64("resume_complete_threshold_ms", 60_000).max(0),
            save_interval: std::time::Duration::from_secs_f64(if interval.is_finite() {
                interval.clamp(2.0, 86_400.0)
            } else {
                15.0
            }),
        }
    }

    pub fn completed(&self, position_ms: i64, duration_ms: i64) -> bool {
        duration_ms > 0
            && position_ms > 0
            && duration_ms.saturating_sub(position_ms) <= self.complete_threshold_ms
    }

    pub fn resume_target(&self, state: &PlaybackState, url: &str) -> i64 {
        let seekable_file =
            crate::media::is_media_file_url(url) && !url.to_ascii_lowercase().contains(".m3u8");
        if !self.enabled
            || state.completed
            || state.position_ms < self.min_ms
            || state.seek_supported == Some(false) && !seekable_file
            || self.completed(state.position_ms, state.duration_ms.unwrap_or(0))
        {
            return 0;
        }
        if state.position_ms > self.back_ms {
            state.position_ms - self.back_ms
        } else {
            state.position_ms.max(0)
        }
    }

    pub fn save_state(
        &self,
        db: &Db,
        article_id: &str,
        url: &str,
        state: &PlaybackState,
    ) -> rusqlite::Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        let mut state = state.clone();
        state.completed |= self.completed(state.position_ms, state.duration_ms.unwrap_or(0));
        // Backends can briefly report zero during stop/reconnect. Keep both
        // meaningful progress and the completed marker instead of erasing them.
        if !state.completed
            && state.position_ms < 2_000
            && saved_state(db, article_id, url)?
                .is_some_and(|old| old.completed || old.position_ms >= 2_000)
        {
            return Ok(false);
        }
        if state.completed {
            state.position_ms = 0;
        }
        db.upsert_playback_state(&playback_id(article_id), &state)?;
        Ok(true)
    }
}

/// Format a millisecond position the way Python's player does
/// (`_format_time` → `_format_chapter_timestamp`): `H:MM:SS`
/// once an hour is reached, else `MM:SS`.
pub fn format_time_ms(ms: i64) -> String {
    let total = (ms.max(0)) / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// The landed-seek summary Python announces once a burst of
/// seek presses settles (`_compose_time_info`): elapsed,
/// remaining and total, or elapsed only for a live stream /
/// unknown length.
pub fn seek_summary(position_ms: i64, duration_ms: i64) -> String {
    let elapsed = format_time_ms(position_ms);
    if duration_ms <= 0 {
        return format!("Elapsed {elapsed}");
    }
    let remaining = format_time_ms(duration_ms - position_ms.max(0));
    let total = format_time_ms(duration_ms);
    format!("Elapsed {elapsed}, Remaining {remaining}, Total {total}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(position_ms: i64) -> PlaybackState {
        PlaybackState {
            position_ms,
            duration_ms: Some(600_000),
            ..PlaybackState::default()
        }
    }

    #[test]
    fn seek_summary_matches_python_compose_time_info() {
        // M10's debounced landed-seek summary: the same words as
        // Python's time field — elapsed, remaining, total.
        assert_eq!(
            seek_summary(65_000, 240_000),
            "Elapsed 01:05, Remaining 02:55, Total 04:00"
        );
        assert_eq!(
            seek_summary(3_661_000, 7_200_000),
            "Elapsed 1:01:01, Remaining 58:59, Total 2:00:00"
        );
        // Unknown length (live stream): elapsed only.
        assert_eq!(seek_summary(65_000, 0), "Elapsed 01:05");
        assert_eq!(seek_summary(0, 0), "Elapsed 00:00");
        // Position past the end never makes remaining negative.
        assert_eq!(
            seek_summary(300_000, 240_000),
            "Elapsed 05:00, Remaining 00:00, Total 04:00"
        );
    }

    #[test]
    fn format_time_ms_matches_python_format_time() {
        assert_eq!(format_time_ms(0), "00:00");
        assert_eq!(format_time_ms(59_000), "00:59");
        assert_eq!(format_time_ms(60_000), "01:00");
        assert_eq!(format_time_ms(3_599_000), "59:59");
        assert_eq!(format_time_ms(3_600_000), "1:00:00");
        assert_eq!(format_time_ms(-5_000), "00:00");
    }

    #[test]
    fn resumes_python_article_ids_then_legacy_ids_and_url() {
        let db = Db::open_or_create(std::path::Path::new(":memory:")).unwrap();
        db.upsert_playback_state("https://example.test/media.mp3", &state(60_000))
            .unwrap();
        assert_eq!(
            saved_state(&db, "42", "https://example.test/media.mp3").unwrap(),
            Some(state(60_000))
        );
        db.upsert_playback_state("42", &state(90_000)).unwrap();
        assert_eq!(
            saved_state(&db, "42", "https://example.test/media.mp3").unwrap(),
            Some(state(90_000))
        );
        db.upsert_playback_state("article:42", &state(120_000))
            .unwrap();
        assert_eq!(
            saved_state(&db, "42", "https://example.test/media.mp3").unwrap(),
            Some(state(120_000))
        );
        assert_eq!(playback_id("42"), "article:42");
        assert_eq!(db.playback_state("42").unwrap(), Some(state(90_000)));
    }

    #[test]
    fn resumes_with_python_defaults_and_configured_limits() {
        let mut cfg = Config::default();
        let policy = ResumePolicy::from_config(&cfg);
        assert_eq!(
            policy.resume_target(&state(120_000), "https://example.test/media.mp3"),
            110_000
        );
        assert_eq!(
            policy.resume_target(&state(5_000), "https://example.test/media.mp3"),
            5_000
        );
        assert_eq!(
            policy.resume_target(&state(550_000), "https://example.test/media.mp3"),
            0
        );
        assert!(!policy.completed(0, 3_000));
        assert!(policy.completed(550_000, 600_000));
        assert_eq!(policy.save_interval, std::time::Duration::from_secs(15));
        cfg.set_path("resume_min_ms", serde_json::json!(30_000));
        cfg.set_path("resume_back_ms", serde_json::json!(5_000));
        cfg.set_path("resume_complete_threshold_ms", serde_json::json!(1_000));
        cfg.set_path("resume_save_interval_s", serde_json::json!(0.5));
        let policy = ResumePolicy::from_config(&cfg);
        assert_eq!(
            policy.resume_target(&state(20_000), "https://example.test/media.mp3"),
            0
        );
        assert_eq!(
            policy.resume_target(&state(550_000), "https://example.test/media.mp3"),
            545_000
        );
        assert_eq!(policy.save_interval, std::time::Duration::from_secs(2));
        cfg.set_path("resume_playback", serde_json::json!(false));
        assert_eq!(
            ResumePolicy::from_config(&cfg)
                .resume_target(&state(120_000), "https://example.test/media.mp3"),
            0
        );
    }

    #[test]
    fn completed_and_unseekable_streams_do_not_resume() {
        let policy = ResumePolicy::from_config(&Config::default());
        let mut saved = state(120_000);
        saved.completed = true;
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/media.mp3"),
            0
        );
        saved.completed = false;
        saved.seek_supported = Some(false);
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/live.m3u8"),
            0
        );
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/media.mp3?token=1"),
            110_000
        );
    }

    #[test]
    fn saves_canonical_ids_and_preserves_progress_on_backend_reset() {
        let db = Db::open_or_create(std::path::Path::new(":memory:")).unwrap();
        let policy = ResumePolicy::from_config(&Config::default());
        db.upsert_playback_state("42", &state(90_000)).unwrap();
        assert!(!policy.save_state(&db, "42", "", &state(0)).unwrap());
        assert!(policy.save_state(&db, "42", "", &state(120_000)).unwrap());
        assert_eq!(
            db.playback_state("article:42").unwrap(),
            Some(state(120_000))
        );
        assert_eq!(db.playback_state("42").unwrap(), Some(state(90_000)));
        assert!(!policy.save_state(&db, "42", "", &state(0)).unwrap());
        assert_eq!(
            db.playback_state("article:42").unwrap(),
            Some(state(120_000))
        );
        assert!(policy.save_state(&db, "42", "", &state(550_000)).unwrap());
        let done = db.playback_state("article:42").unwrap().unwrap();
        assert!(done.completed);
        assert_eq!(done.position_ms, 0);
        assert!(!policy.save_state(&db, "42", "", &state(0)).unwrap());
        assert!(db.playback_state("article:42").unwrap().unwrap().completed);
    }

    #[test]
    fn disabled_resume_never_changes_saved_progress() {
        let db = Db::open_or_create(std::path::Path::new(":memory:")).unwrap();
        let mut cfg = Config::default();
        cfg.set_path("resume_playback", serde_json::json!(false));
        let policy = ResumePolicy::from_config(&cfg);
        assert!(!policy.save_state(&db, "42", "", &state(120_000)).unwrap());
        assert_eq!(db.playback_state("article:42").unwrap(), None);
    }
}
