//! The one player session's identity/generation policy
//! (Audit 8 M01, M11; Python `gui/mainframe.py` `_ensure_player_window`
//! + `gui/player.py` `load_media`/`is_current_media`).
//!
//! Python owns exactly one player window. Activating the media
//! it already holds toggles play/pause; activating anything else
//! loads into the same window, stopping the old playback. Every
//! load carries a sequence number (`_load_seq`): a resolution
//! that finishes after a newer load was requested is stale and
//! must never start audio. This module is that policy, GUI-free
//! so the app window and its tests share one implementation.

/// What one activation (Enter / Play) means for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// No live session window: create it (in the loading state).
    Create,
    /// A live window already holds this media: toggle play/pause
    /// instead of loading anything (Python's `is_current_media`
    /// branch of `_open_article`).
    Toggle,
    /// A live window holds something else: reuse it — stop the
    /// old playback and load the new media into it.
    Reuse,
}

/// Identity of the media a session is bound to: the article id
/// and the playback target (media URL when known, else the
/// article URL). Matching follows Python's `is_current_media`:
/// equal non-empty article ids match; otherwise equal non-empty
/// target URLs match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionIdentity {
    pub article_id: String,
    pub target: String,
}

impl SessionIdentity {
    pub fn new(article_id: &str, target: &str) -> Self {
        SessionIdentity {
            article_id: article_id.to_string(),
            target: target.to_string(),
        }
    }

    pub fn matches(&self, other: &SessionIdentity) -> bool {
        if !self.article_id.is_empty()
            && !other.article_id.is_empty()
            && self.article_id == other.article_id
        {
            return true;
        }
        !self.target.is_empty() && !other.target.is_empty() && self.target == other.target
    }
}

/// Generation/identity state for the single player session.
/// Generations start at 1; 0 is never current, so a default or
/// reset pending generation can never accept a delivery.
#[derive(Debug, Clone, Default)]
pub struct PlayerSession {
    generation: u64,
    identity: Option<SessionIdentity>,
}

impl PlayerSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn identity(&self) -> Option<&SessionIdentity> {
        self.identity.as_ref()
    }

    /// True when `generation` is the session's live generation:
    /// a worker result stamped with it may still act (start
    /// audio, apply metadata). Anything older is stale.
    pub fn is_current(&self, generation: u64) -> bool {
        generation != 0 && generation == self.generation
    }

    /// Register one activation. `alive` says whether the session
    /// window exists. A toggle keeps the current generation (the
    /// in-flight state is untouched); a new load bumps it, which
    /// is what makes every older pending result stale.
    pub fn activate(&mut self, alive: bool, identity: SessionIdentity) -> (Activation, u64) {
        if alive
            && let Some(current) = &self.identity
            && current.matches(&identity)
        {
            return (Activation::Toggle, self.generation);
        }
        self.generation += 1;
        self.identity = Some(identity);
        (
            if alive {
                Activation::Reuse
            } else {
                Activation::Create
            },
            self.generation,
        )
    }

    /// A load that must happen even when the identity already
    /// matches (starting a play queue on the loaded item):
    /// Python's queue path calls `load_media` directly and never
    /// takes the toggle branch. Always bumps the generation.
    pub fn activate_new_load(
        &mut self,
        alive: bool,
        identity: SessionIdentity,
    ) -> (Activation, u64) {
        self.generation += 1;
        self.identity = Some(identity);
        (
            if alive {
                Activation::Reuse
            } else {
                Activation::Create
            },
            self.generation,
        )
    }

    /// A load that begins inside the session itself (the play
    /// queue advancing on media finish): same bump as an external
    /// activation, so a main-window resolution still in flight
    /// for the previous item goes stale.
    pub fn begin_internal_load(&mut self, identity: SessionIdentity) -> u64 {
        self.generation += 1;
        self.identity = Some(identity);
        self.generation
    }

    /// The window closed: forget the identity. The generation is
    /// kept (not reset) so results stamped before the close can
    /// never match a future session either.
    pub fn close(&mut self) {
        self.identity = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(article: &str, target: &str) -> SessionIdentity {
        SessionIdentity::new(article, target)
    }

    #[test]
    fn first_activation_creates_with_generation_one() {
        let mut s = PlayerSession::new();
        let (kind, g) = s.activate(false, id("a1", "https://x/1.mp3"));
        assert_eq!(kind, Activation::Create);
        assert_eq!(g, 1);
        assert!(s.is_current(1));
        assert!(!s.is_current(0));
    }

    #[test]
    fn repeated_activation_of_current_media_toggles() {
        // M01: pressing Enter again on the playing item must not
        // spawn a second load (or a second player) — it toggles.
        let mut s = PlayerSession::new();
        let (_, g) = s.activate(false, id("a1", "https://x/1.mp3"));
        let (kind, gen2) = s.activate(true, id("a1", "https://x/1.mp3"));
        assert_eq!(kind, Activation::Toggle);
        assert_eq!(gen2, g, "a toggle must not disturb the generation");
        assert!(s.is_current(g));
    }

    #[test]
    fn switching_source_reuses_and_stales_the_old_generation() {
        // M01/M11: a slow resolution for the previous item must
        // never start audio once a newer item was requested.
        let mut s = PlayerSession::new();
        let (_, gen1) = s.activate(false, id("a1", "https://x/1.mp3"));
        let (kind, gen2) = s.activate(true, id("a2", "https://x/2.mp3"));
        assert_eq!(kind, Activation::Reuse);
        assert!(gen2 > gen1);
        assert!(!s.is_current(gen1), "stale resolution must be dropped");
        assert!(s.is_current(gen2));
    }

    #[test]
    fn identity_matches_by_id_even_when_targets_differ() {
        // Python is_current_media: an id match wins even when the
        // freshly resolved target differs from the loaded one.
        let mut s = PlayerSession::new();
        s.activate(false, id("a1", "https://cdn/old.mp3"));
        let (kind, _) = s.activate(true, id("a1", "https://cdn/new.mp3"));
        assert_eq!(kind, Activation::Toggle);
    }

    #[test]
    fn identity_matches_by_target_when_ids_are_absent() {
        // Pasted media URLs carry the URL as their only identity.
        let mut s = PlayerSession::new();
        s.activate(false, id("", "https://x/clip.mp3"));
        let (kind, _) = s.activate(true, id("", "https://x/clip.mp3"));
        assert_eq!(kind, Activation::Toggle);
        let (kind, _) = s.activate(true, id("", "https://x/other.mp3"));
        assert_eq!(kind, Activation::Reuse);
    }

    #[test]
    fn empty_identity_never_matches() {
        let mut s = PlayerSession::new();
        s.activate(false, id("", ""));
        let (kind, _) = s.activate(true, id("", ""));
        assert_eq!(kind, Activation::Reuse);
    }

    #[test]
    fn forced_load_never_toggles() {
        // Starting a queue on the currently loaded item reloads
        // it (with queue context) instead of pausing it.
        let mut s = PlayerSession::new();
        let (_, gen1) = s.activate(false, id("a1", "https://x/1.mp3"));
        let (kind, gen2) = s.activate_new_load(true, id("a1", "https://x/1.mp3"));
        assert_eq!(kind, Activation::Reuse);
        assert!(gen2 > gen1);
        assert!(!s.is_current(gen1));
        let (kind, _) = s.activate_new_load(false, id("a1", "https://x/1.mp3"));
        assert_eq!(kind, Activation::Create);
    }

    #[test]
    fn internal_load_stales_a_pending_external_result() {
        // The queue advancing on its own supersedes a resolution
        // the main window still has in flight for the old item.
        let mut s = PlayerSession::new();
        let (_, gen1) = s.activate(false, id("a1", "https://x/1.mp3"));
        let gen2 = s.begin_internal_load(id("a2", "https://x/2.mp3"));
        assert!(!s.is_current(gen1));
        assert!(s.is_current(gen2));
    }

    #[test]
    fn close_forgets_identity_but_keeps_generations_moving() {
        let mut s = PlayerSession::new();
        let (_, gen1) = s.activate(false, id("a1", "https://x/1.mp3"));
        s.close();
        // Same media after a close is a fresh create, not a toggle.
        let (kind, gen2) = s.activate(false, id("a1", "https://x/1.mp3"));
        assert_eq!(kind, Activation::Create);
        assert!(gen2 > gen1);
        assert!(!s.is_current(gen1));
    }
}
