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

/// Backend stream phase as the seek verifier needs it. Python's
/// guard waits while VLC reports Opening (1) or Buffering (2);
/// every other state counts as ready to judge a landing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamPhase {
    Opening,
    Ready,
}

/// Which seek a [`SeekVerifier`] is watching: an automatic resume
/// restore (Python's `_maybe_restore_playback_position` tick) or a
/// user seek (Python's seek guard, `_start_seek_guard` /
/// `_seek_guard_tick`). The two share one landing/tolerance model
/// but differ in retry budget and failure behaviour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeekKind {
    Resume,
    User,
}

/// What one verifier observation decided. `seek_to` asks the
/// caller to issue (or re-issue) a backend seek; `landed` ends the
/// verification successfully; `learn_seek_supported` carries a
/// seek-support fact the caller must persist; `announce_failure`
/// is set exactly once, when a user seek gives up far from its
/// target (Python's `_announce_seek_not_reached` discipline).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SeekStep {
    pub seek_to: Option<i64>,
    pub landed: bool,
    pub learn_seek_supported: Option<bool>,
    pub announce_failure: bool,
    pub active: bool,
}

/// Verified seek state (Audit 8 M05). A seek request is never
/// assumed to have landed: the verifier watches the backend's
/// real position, waits through opening/buffering, retries within
/// a bounded budget, learns seek support from the outcome, and
/// reports failure at most once. Pure and clock-injected so the
/// whole policy is unit-testable without a media backend.
///
/// Python's numbers, kept: resume landing tolerance 1500 ms;
/// resume issues at most one seek and gives up silently after
/// 8 s, marking the stream unseekable only when the backend
/// itself reports unseekable and the input does not look
/// seekable; the user-seek guard has 10 attempts and 3 reapplies,
/// tolerance 3000 ms local / 5000 ms remote, remote gives up once
/// attempts drop below 7, progress toward the target within the
/// last 0.9 s pauses the budget, and the failure announcement
/// fires only when the remaining gap is at least 8000 ms.
pub struct SeekVerifier {
    kind: SeekKind,
    target_ms: i64,
    remote: bool,
    seekable_looking: bool,
    started: std::time::Instant,
    attempts_made: u32,
    guard_attempts_left: u32,
    reapplies_left: u32,
    best_distance_ms: i64,
    last_progress_at: std::time::Instant,
    done: bool,
}

impl SeekVerifier {
    pub fn for_resume(
        target_ms: i64,
        remote: bool,
        seekable_looking: bool,
        now: std::time::Instant,
    ) -> Self {
        Self {
            kind: SeekKind::Resume,
            target_ms: target_ms.max(0),
            remote,
            seekable_looking,
            started: now,
            attempts_made: 0,
            guard_attempts_left: 0,
            reapplies_left: 0,
            best_distance_ms: i64::MAX,
            last_progress_at: now,
            done: false,
        }
    }

    pub fn for_user_seek(target_ms: i64, remote: bool, now: std::time::Instant) -> Self {
        Self {
            kind: SeekKind::User,
            target_ms: target_ms.max(0),
            remote,
            seekable_looking: true,
            started: now,
            attempts_made: 1, // the caller issues the seek itself
            guard_attempts_left: 10,
            reapplies_left: 3,
            best_distance_ms: i64::MAX,
            last_progress_at: now,
            done: false,
        }
    }

    pub fn kind(&self) -> SeekKind {
        self.kind
    }

    pub fn target_ms(&self) -> i64 {
        self.target_ms
    }

    pub fn is_active(&self) -> bool {
        !self.done
    }

    /// Abandon verification (a user seek supersedes a pending
    /// resume — Python `_note_user_seek`; a new load supersedes
    /// both). No landing, no learning, no announcement.
    pub fn cancel(&mut self) {
        self.done = true;
    }

    pub fn observe(
        &mut self,
        now: std::time::Instant,
        phase: StreamPhase,
        position_ms: i64,
        backend_seekable: Option<bool>,
    ) -> SeekStep {
        let mut step = SeekStep {
            active: !self.done,
            ..SeekStep::default()
        };
        if self.done || phase == StreamPhase::Opening {
            return step;
        }
        match self.kind {
            SeekKind::Resume => self.observe_resume(now, position_ms, backend_seekable, &mut step),
            SeekKind::User => self.observe_user(now, position_ms, &mut step),
        }
        step.active = !self.done;
        step
    }

    fn observe_resume(
        &mut self,
        now: std::time::Instant,
        position_ms: i64,
        backend_seekable: Option<bool>,
        step: &mut SeekStep,
    ) {
        if position_ms >= 0 && (position_ms - self.target_ms).abs() <= 1_500 {
            self.done = true;
            step.landed = true;
            step.learn_seek_supported = Some(true);
            return;
        }
        if self.attempts_made == 0 {
            step.seek_to = Some(self.target_ms);
            self.attempts_made = 1;
            return;
        }
        // One bounded attempt (Python resume_max_attempts = 1);
        // after 8 s without landing, give up silently — unless
        // the backend itself says the stream cannot seek and the
        // input never looked seekable, which is learned.
        if now.duration_since(self.started) >= std::time::Duration::from_secs(8) {
            self.done = true;
            if backend_seekable == Some(false) && !self.seekable_looking {
                step.learn_seek_supported = Some(false);
            }
        }
    }

    fn observe_user(&mut self, now: std::time::Instant, position_ms: i64, step: &mut SeekStep) {
        let tolerance = if self.remote { 5_000 } else { 3_000 };
        let distance = if position_ms >= 0 {
            (position_ms - self.target_ms).abs()
        } else {
            i64::MAX
        };
        if position_ms >= 0 && distance <= tolerance {
            self.done = true;
            step.landed = true;
            return;
        }
        // Progress toward the target (> 200 ms closer) resets the
        // stall clock; while progress is fresh (< 0.9 s) the guard
        // waits instead of spending attempts.
        if position_ms >= 0 && distance <= self.best_distance_ms - 200 {
            self.best_distance_ms = distance;
            self.last_progress_at = now;
        }
        if self.attempts_made > 0
            && now.duration_since(self.last_progress_at) < std::time::Duration::from_millis(900)
            && self.best_distance_ms != i64::MAX
        {
            return;
        }
        if self.guard_attempts_left == 0 || (self.remote && self.guard_attempts_left < 7) {
            self.done = true;
            step.announce_failure = position_ms >= 0 && distance >= 8_000;
            return;
        }
        self.guard_attempts_left -= 1;
        // Re-apply only when Python would (`_should_reapply_seek`):
        // retries remain and the position is unknown or outside
        // tolerance.
        if self.reapplies_left > 0 && (position_ms < 0 || distance > tolerance) {
            self.reapplies_left -= 1;
            self.attempts_made += 1;
            step.seek_to = Some(self.target_ms);
        }
    }
}

/// Latest-wins queue of playback-state writes (Audit 8 M07),
/// modelling Python's `_pending_resume_db_upserts` /
/// `_pending_seek_supported_updates` dicts. The player enqueues
/// instead of writing on the UI thread; a worker flushes through
/// [`PendingPlaybackWrites::flush`]. A write that fails (a locked
/// database above all) stays queued for the next flush — it is
/// never dropped — and flush reports exactly which saves landed
/// so the caller advances its save bookkeeping only past written
/// state.
#[derive(Default)]
pub struct PendingPlaybackWrites {
    saves: std::collections::HashMap<String, PendingSave>,
    seek_support: std::collections::HashMap<String, bool>,
}

#[derive(Clone)]
struct PendingSave {
    article_id: String,
    url: String,
    state: PlaybackState,
}

impl PendingPlaybackWrites {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a position save for an article; a newer save for the
    /// same article replaces the queued one (latest wins).
    pub fn queue_save(&mut self, article_id: &str, url: &str, state: PlaybackState) {
        self.saves.insert(
            playback_id(article_id),
            PendingSave {
                article_id: article_id.to_string(),
                url: url.to_string(),
                state,
            },
        );
    }

    /// Queue a learned seek-support update, keyed by playback id.
    pub fn queue_seek_supported(&mut self, playback_id: &str, supported: bool) {
        self.seek_support.insert(playback_id.to_string(), supported);
    }

    pub fn is_empty(&self) -> bool {
        self.saves.is_empty() && self.seek_support.is_empty()
    }

    pub fn pending_len(&self) -> usize {
        self.saves.len() + self.seek_support.len()
    }

    /// Attempt every queued write against `db`. Completed writes
    /// (including saves the policy deliberately skips, which are
    /// decisions, not failures) are removed and the saves among
    /// them returned as (playback id, state); failed writes stay
    /// queued for the next flush.
    pub fn flush(&mut self, db: &Db, policy: &ResumePolicy) -> Vec<(String, PlaybackState)> {
        let mut written = Vec::new();
        let save_ids: Vec<String> = self.saves.keys().cloned().collect();
        for id in save_ids {
            let Some(save) = self.saves.get(&id).cloned() else {
                continue;
            };
            match policy.save_state(db, &save.article_id, &save.url, &save.state) {
                Ok(_) => {
                    self.saves.remove(&id);
                    written.push((id, save.state));
                }
                Err(_) => { /* stays queued; retried on the next flush */ }
            }
        }
        let support_ids: Vec<String> = self.seek_support.keys().cloned().collect();
        for id in support_ids {
            let Some(supported) = self.seek_support.get(&id).copied() else {
                continue;
            };
            if db.set_seek_supported(&id, supported).is_ok() {
                self.seek_support.remove(&id);
            }
        }
        written
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

    // ---- SeekVerifier (M05) ------------------------------------

    fn at(base: std::time::Instant, ms: u64) -> std::time::Instant {
        base + std::time::Duration::from_millis(ms)
    }

    #[test]
    fn resume_waits_through_buffering_then_seeks_once() {
        // Delayed length/Buffering: while the backend reports
        // Opening the verifier must not seek at all; once ready it
        // issues exactly one seek (Python resume_max_attempts = 1).
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_resume(600_000, false, true, t0);
        for ms in [0, 400, 900] {
            let step = v.observe(at(t0, ms), StreamPhase::Opening, -1, None);
            assert_eq!(step.seek_to, None, "seeked while opening at {ms} ms");
            assert!(step.active);
        }
        let step = v.observe(at(t0, 1_200), StreamPhase::Ready, 0, None);
        assert_eq!(step.seek_to, Some(600_000));
        // The fake backend rejects the seek (position stays at 0):
        // no second attempt is ever issued.
        for ms in [2_000, 4_000, 7_000] {
            let step = v.observe(at(t0, ms), StreamPhase::Ready, 0, None);
            assert_eq!(step.seek_to, None, "resume re-sought at {ms} ms");
            assert!(step.active, "gave up before 8 s at {ms} ms");
        }
    }

    #[test]
    fn resume_landing_within_tolerance_learns_seekable() {
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_resume(600_000, false, true, t0);
        let step = v.observe(at(t0, 100), StreamPhase::Ready, 0, None);
        assert_eq!(step.seek_to, Some(600_000));
        let step = v.observe(at(t0, 900), StreamPhase::Ready, 599_000, None);
        assert!(step.landed);
        assert_eq!(step.learn_seek_supported, Some(true));
        assert!(!step.active);
        assert!(!v.is_active());
    }

    #[test]
    fn resume_gives_up_after_eight_seconds_and_learns_unseekable() {
        // Fake rejected seek + backend reports unseekable + input
        // never looked seekable: after 8 s the restore ends and
        // seek support is learned as false (Python's stall rule).
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_resume(600_000, false, false, t0);
        let step = v.observe(at(t0, 100), StreamPhase::Ready, 0, Some(false));
        assert_eq!(step.seek_to, Some(600_000));
        let step = v.observe(at(t0, 8_500), StreamPhase::Ready, 0, Some(false));
        assert!(!step.active);
        assert!(!step.landed);
        assert_eq!(step.learn_seek_supported, Some(false));
        assert!(!step.announce_failure, "resume give-up is silent");
    }

    #[test]
    fn resume_never_marks_seekable_looking_input_unseekable() {
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_resume(600_000, false, true, t0);
        v.observe(at(t0, 100), StreamPhase::Ready, 0, Some(false));
        let step = v.observe(at(t0, 9_000), StreamPhase::Ready, 0, Some(false));
        assert!(!step.active);
        assert_eq!(step.learn_seek_supported, None);
    }

    #[test]
    fn user_seek_cancels_a_pending_resume() {
        // Python `_note_user_seek`: a user seek while a restore is
        // inflight resets the restore — the verifier goes inactive
        // and never seeks, lands or learns again.
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_resume(600_000, false, true, t0);
        v.cancel();
        assert!(!v.is_active());
        let step = v.observe(at(t0, 100), StreamPhase::Ready, 600_000, None);
        assert_eq!(step, SeekStep::default());
    }

    #[test]
    fn user_seek_guard_reapplies_boundedly_then_speaks_once() {
        // Fake rejected user seek: the guard re-applies at most 3
        // times, spends at most 10 attempts, then reports failure
        // exactly once because the gap is far over 8000 ms.
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_user_seek(300_000, false, t0);
        let mut reapplies = 0;
        let mut failure_at = None;
        for i in 0..40u64 {
            let step = v.observe(at(t0, i * 1_000), StreamPhase::Ready, 0, None);
            if step.seek_to.is_some() {
                reapplies += 1;
            }
            if step.announce_failure {
                failure_at = Some(i);
                break;
            }
            assert!(!step.landed);
        }
        assert_eq!(reapplies, 3, "guard must re-apply exactly 3 times");
        assert!(failure_at.is_some(), "guard never reported failure");
        // Failure is once: the verifier is done afterwards.
        let step = v.observe(at(t0, 60_000), StreamPhase::Ready, 0, None);
        assert_eq!(step, SeekStep::default());
    }

    #[test]
    fn user_seek_guard_lands_within_remote_tolerance() {
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_user_seek(300_000, true, t0);
        // 4.5 s short of the target: outside the 3000 ms local
        // tolerance but inside the 5000 ms remote one.
        let step = v.observe(at(t0, 100), StreamPhase::Ready, 295_500, None);
        assert!(step.landed);
        assert!(!step.active);
    }

    #[test]
    fn user_seek_guard_quiet_when_failure_gap_is_small() {
        // Budget exhausted but the position ended within 8000 ms
        // of the target: Python stays silent.
        let t0 = std::time::Instant::now();
        let mut v = SeekVerifier::for_user_seek(300_000, false, t0);
        let mut announced = false;
        for i in 0..40u64 {
            let step = v.observe(at(t0, i * 1_000), StreamPhase::Ready, 294_000, None);
            announced |= step.announce_failure;
            if !step.active {
                break;
            }
        }
        assert!(!announced);
        assert!(!v.is_active());
    }

    #[test]
    fn stop_then_play_target_is_saved_position_minus_rewind() {
        // M08's policy half: Play after Stop re-runs the restore
        // against the saved row — ten minutes saved, configured
        // 30 s rewind, restarts at 9:30.
        let mut cfg = Config::default();
        cfg.set_path("resume_back_ms", serde_json::json!(30_000));
        let policy = ResumePolicy::from_config(&cfg);
        let mut saved = state(600_000);
        saved.duration_ms = Some(3_600_000);
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/media.mp3"),
            570_000
        );
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
