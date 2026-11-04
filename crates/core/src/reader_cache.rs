//! Bounded on-demand reader text cache; background prefetch is deliberately disabled.

use std::collections::VecDeque;

#[derive(Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
