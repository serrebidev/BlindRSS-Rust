//! Bounded on-demand reader text cache; background prefetch is deliberately disabled.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderRequest {
    pub key: String,
    pub article_id: String,
    pub url: String,
    pub generation: u64,
}

impl ReaderRequest {
    pub fn is_current(&self, generation: u64, article_id: &str, url: &str) -> bool {
        self.generation == generation && self.article_id == article_id && self.url == url
    }
}

/// Identity of an in-flight article translation (Audit 8
/// O08): the article it was requested for, the reader
/// generation at request time, and the translation variant
/// (`translate::TranslationSettings::cache_suffix` — provider,
/// target language and configured model). A result applies
/// only while all three still match the live state: a delayed
/// result for article A must never replace article B's text,
/// and a provider/language/model (or account) change
/// invalidates results requested under the old variant.
/// Python guards the same triple in
/// `MainFrame._fulltext_apply_result` (token + current cache
/// key, whose suffix carries the variant).
#[derive(Clone)]
pub struct TranslationRequest {
    pub article_id: String,
    pub generation: u64,
    pub variant: String,
}

impl TranslationRequest {
    pub fn is_current(&self, generation: u64, article_id: &str, variant: &str) -> bool {
        self.generation == generation && self.article_id == article_id && self.variant == variant
    }
}

pub struct ReaderCache {
    entries: VecDeque<(String, String, &'static str, usize, u64)>,
    max_entries: usize,
    max_chars: usize,
    chars: usize,
}

impl Default for ReaderCache {
    fn default() -> Self {
        Self::new(300, 8_000_000)
    }
}

impl ReaderCache {
    pub fn new(max_entries: usize, max_chars: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
            max_chars,
            chars: 0,
        }
    }

    pub fn get(&mut self, key: &str, require_web: bool) -> Option<(String, &'static str)> {
        let index = self.entries.iter().position(|(k, _, source, ..)| {
            k == key && (!require_web || !matches!(*source, "provider" | "miniflux"))
        })?;
        let entry = self.entries.remove(index)?;
        let result = (entry.1.clone(), entry.2);
        self.entries.push_back(entry);
        Some(result)
    }

    /// Drop every entry whose key starts with `prefix`
    /// (Audit 8 G16: reader cache keys begin with the article
    /// id, so an image-alt change invalidates exactly that
    /// article's cached renderings before the rerender).
    pub fn remove_prefix(&mut self, prefix: &str) {
        let mut removed_chars = 0usize;
        self.entries.retain(|entry| {
            if entry.0.starts_with(prefix) {
                removed_chars += entry.3;
                false
            } else {
                true
            }
        });
        self.chars = self.chars.saturating_sub(removed_chars);
    }

    pub fn insert(&mut self, key: String, text: String, source: &'static str, generation: u64) {
        if self
            .entries
            .iter()
            .any(|entry| entry.0 == key && entry.4 > generation)
        {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(k, ..)| k == &key)
            && let Some(old) = self.entries.remove(index)
        {
            self.chars -= old.3;
        }
        let count = text.chars().count();
        if source == "feed" || text.trim().is_empty() || count > self.max_chars {
            return;
        }
        self.entries
            .push_back((key, text, source, count, generation));
        self.chars += count;
        while self.entries.len() > self.max_entries || self.chars > self.max_chars {
            if let Some(old) = self.entries.pop_front() {
                self.chars -= old.3;
            }
        }
    }
}

pub enum RichLoad {
    Cached(ReaderRequest, String),
    Fetch(ReaderRequest),
}

/// On-demand HTML cache and debounced selection; workers never own UI state.
#[derive(Default)]
pub struct RichReaderCache {
    cache: ReaderCache,
    desired: Option<ReaderRequest>,
    pending: Option<(ReaderRequest, Instant)>,
    in_flight: Vec<ReaderRequest>,
}

impl RichReaderCache {
    pub fn select(&mut self, request: ReaderRequest, now: Instant) -> Option<String> {
        self.desired = Some(request.clone());
        if let Some((html, _)) = self.cache.get(&request.key, false) {
            self.pending = None;
            return Some(html);
        }
        if !self
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| pending == &request)
        {
            self.pending = Some((request, now + Duration::from_millis(180)));
        }
        None
    }

    pub fn take_ready(&mut self, now: Instant) -> Option<RichLoad> {
        let (request, ready_at) = self.pending.as_ref()?;
        if now < *ready_at {
            return None;
        }
        if let Some((html, _)) = self.cache.get(&request.key, false) {
            let (request, _) = self.pending.take()?;
            return Some(RichLoad::Cached(request, html));
        }
        if self
            .in_flight
            .iter()
            .any(|active| active.key == request.key)
        {
            return None;
        }
        let (request, _) = self.pending.take()?;
        self.in_flight.push(request.clone());
        Some(RichLoad::Fetch(request))
    }

    pub fn finish(&mut self, request: &ReaderRequest, html: Option<String>) -> Option<String> {
        let index = self.in_flight.iter().position(|active| active == request)?;
        self.in_flight.remove(index);
        let current = self.desired.as_ref() == Some(request);
        if current {
            self.pending = None;
        }
        let html = html.filter(|html| !html.trim().is_empty())?;
        self.cache.insert(
            request.key.clone(),
            html.clone(),
            "rich",
            request.generation,
        );
        current.then_some(html)
    }

    pub fn cancel_selection(&mut self) {
        self.desired = None;
        self.pending = None;
    }

    pub fn clear(&mut self) {
        self.cache = ReaderCache::default();
        self.cancel_selection();
        self.in_flight.clear();
    }

    pub fn remove_prefix(&mut self, prefix: &str) {
        self.cache.remove_prefix(prefix);
        self.in_flight
            .retain(|request| !request.key.starts_with(prefix));
        if self
            .desired
            .as_ref()
            .is_some_and(|request| request.key.starts_with(prefix))
        {
            self.cancel_selection();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rich_request(id: &str, generation: u64) -> ReaderRequest {
        ReaderRequest {
            key: format!("{id}:body:settings"),
            article_id: id.into(),
            url: format!("https://example.com/{id}"),
            generation,
        }
    }

    fn fetch(cache: &mut RichReaderCache, now: std::time::Instant) -> ReaderRequest {
        match cache.take_ready(now).expect("ready request") {
            RichLoad::Fetch(request) => request,
            RichLoad::Cached(..) => panic!("unexpected cached document"),
        }
    }

    #[test]
    fn rich_selection_debounces_to_latest_without_blocking_on_old_work() {
        let now = std::time::Instant::now();
        let later = now + std::time::Duration::from_millis(180);
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        assert!(cache.take_ready(now).is_none());
        cache.select(rich_request("b", 2), now);
        assert_eq!(fetch(&mut cache, later).article_id, "b");
        cache.select(rich_request("c", 3), later);
        assert_eq!(fetch(&mut cache, later + (later - now)).article_id, "c");
    }

    #[test]
    fn rich_duplicate_request_keeps_debounce_and_fetches_once() {
        let now = std::time::Instant::now();
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        cache.select(
            rich_request("a", 1),
            now + std::time::Duration::from_millis(100),
        );
        let request = fetch(&mut cache, now + std::time::Duration::from_millis(180));
        cache.select(rich_request("a", 1), now);
        assert!(
            cache
                .take_ready(now + std::time::Duration::from_secs(1))
                .is_none()
        );
        assert_eq!(
            cache.finish(&request, Some("html".into())),
            Some("html".into())
        );
        assert_eq!(cache.select(rich_request("a", 2), now), Some("html".into()));
    }

    #[test]
    fn rich_old_a_result_is_cached_but_applies_only_with_current_generation() {
        let now = std::time::Instant::now();
        let ready = now + std::time::Duration::from_secs(1);
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        let old_a = fetch(&mut cache, ready);
        cache.select(rich_request("b", 2), now);
        cache.select(rich_request("a", 3), now);
        assert!(cache.take_ready(ready).is_none());
        assert!(cache.finish(&old_a, Some("html".into())).is_none());
        match cache.take_ready(ready).expect("current cached request") {
            RichLoad::Cached(request, html) => {
                assert_eq!(request.generation, 3);
                assert_eq!(html, "html");
            }
            RichLoad::Fetch(_) => panic!("duplicate fetch"),
        }
    }

    #[test]
    fn rich_failure_retries_on_next_request() {
        let now = std::time::Instant::now();
        let ready = now + std::time::Duration::from_secs(1);
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        let request = fetch(&mut cache, ready);
        assert!(cache.finish(&request, None).is_none());
        assert!(cache.take_ready(ready).is_none());
        assert!(cache.select(rich_request("a", 1), now).is_none());
        assert_eq!(fetch(&mut cache, ready).article_id, "a");
    }

    #[test]
    fn rich_invalidation_discards_stale_completion_without_consuming_new_request() {
        let now = std::time::Instant::now();
        let ready = now + std::time::Duration::from_secs(1);
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        let old = fetch(&mut cache, ready);
        cache.clear();
        cache.select(rich_request("a", 2), now);
        let new = fetch(&mut cache, ready);
        assert!(cache.finish(&old, Some("old".into())).is_none());
        assert_eq!(cache.finish(&new, Some("new".into())), Some("new".into()));
        cache.remove_prefix("a:");
        assert!(cache.select(rich_request("a", 3), now).is_none());
        let request = fetch(&mut cache, ready);
        cache.remove_prefix("a:");
        assert!(cache.finish(&request, Some("stale".into())).is_none());
        assert!(cache.select(rich_request("a", 4), now).is_none());
    }

    #[test]
    fn rich_cancelled_selection_still_caches_completed_work() {
        let now = Instant::now();
        let ready = now + Duration::from_secs(1);
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        let request = fetch(&mut cache, ready);
        cache.select(rich_request("b", 2), now);
        cache.cancel_selection();
        assert!(cache.take_ready(ready).is_none());
        assert!(cache.finish(&request, Some("html".into())).is_none());
        assert_eq!(cache.select(rich_request("a", 3), now), Some("html".into()));
    }

    #[test]
    fn rich_changed_body_does_not_accept_or_reuse_old_document() {
        let now = Instant::now();
        let ready = now + Duration::from_secs(1);
        let mut cache = RichReaderCache::default();
        cache.select(rich_request("a", 1), now);
        let old = fetch(&mut cache, ready);
        let mut changed = rich_request("a", 1);
        changed.key.push_str(":changed-body");
        cache.select(changed, now);
        assert!(cache.finish(&old, Some("old body".into())).is_none());
        let request = fetch(&mut cache, ready);
        assert_eq!(
            cache.finish(&request, Some("new body".into())),
            Some("new body".into())
        );
    }

    #[test]
    fn successful_loads_are_lru_bounded_and_feed_fallbacks_retry() {
        let mut cache = ReaderCache::new(2, 10);
        cache.insert("a".into(), "aaa".into(), "web", 0);
        cache.insert("b".into(), "bbb".into(), "provider", 0);
        assert!(cache.get("a", false).is_some());
        cache.insert("c".into(), "ccc".into(), "web", 0);
        assert!(cache.get("b", false).is_none());
        cache.insert("fallback".into(), "feed".into(), "feed", 0);
        assert!(cache.get("fallback", false).is_none());
        cache.insert("large".into(), "01234567890".into(), "web", 0);
        assert!(cache.get("large", false).is_none());
        cache.insert("p".into(), "p".into(), "provider", 0);
        assert!(cache.get("p", true).is_none());
        assert!(cache.get("p", false).is_some());
    }

    #[test]
    fn character_budget_counts_unicode_and_replacements() {
        let mut cache = ReaderCache::new(3, 4);
        cache.insert("a".into(), "éé".into(), "web", 0);
        cache.insert("b".into(), "bbb".into(), "web", 0);
        assert!(cache.get("a", false).is_none());
        cache.insert("b".into(), "b".into(), "web", 0);
        cache.insert("c".into(), "ccc".into(), "web", 0);
        assert!(cache.get("b", false).is_some());
    }

    #[test]
    fn late_old_request_cannot_replace_newer_cached_result() {
        let mut cache = ReaderCache::default();
        cache.insert("a".into(), "new".into(), "web", 3);
        cache.insert("a".into(), "old".into(), "web", 1);
        assert_eq!(cache.get("a", false).unwrap().0, "new");
        cache.insert("a".into(), "feed".into(), "feed", 2);
        assert_eq!(cache.get("a", false).unwrap().0, "new");
    }

    #[test]
    fn request_cannot_apply_after_selection_or_url_changes() {
        let request = ReaderRequest {
            key: "key".into(),
            article_id: "a".into(),
            url: "url".into(),
            generation: 1,
        };
        assert!(request.is_current(1, "a", "url"));
        assert!(!request.is_current(2, "a", "url"));
        assert!(!request.is_current(1, "b", "url"));
        assert!(!request.is_current(1, "a", "changed"));
    }
}
