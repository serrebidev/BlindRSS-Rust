//! Case-insensitive text search with Python's exact caret and
//! wrap semantics, shared by the article reader (Audit 8 G14,
//! `gui/mainframe.py::_find_in_text`) and the help viewer
//! (Audit 8 S07, `gui/help_viewer.py::_find`).
//!
//! All positions are character indices (Python string
//! indices), never byte offsets or display-line coordinates.

fn lower_chars(text: &str) -> Vec<char> {
    text.to_lowercase().chars().collect()
}

fn find_from(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Last match that ends at or before `end` (Python
/// `str.rfind(needle, 0, end)` semantics: the match must fit
/// entirely inside `[0, end)`).
fn rfind_within(hay: &[char], needle: &[char], end: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last_start = (hay.len() - needle.len()).min(end.saturating_sub(needle.len()));
    if end < needle.len() {
        return None;
    }
    (0..=last_start)
        .rev()
        .find(|&i| hay[i..i + needle.len()] == *needle)
}

fn rfind_all(hay: &[char], needle: &[char]) -> Option<usize> {
    rfind_within(hay, needle, hay.len())
}

/// Python `_find_in_text`: find `term` in `text`
/// case-insensitively, returning the match's (start, end)
/// character positions.
///
/// Forward: the first match at index >= `start`, else (with
/// `wrap`) the first match from the top. Backward: the last
/// match ending at or before `start`, else (with `wrap`) the
/// last match in the text. The reader's F3/Shift+F3 pass
/// `wrap: false` on purpose — reaching the end says so instead
/// of silently jumping back (Python's issue-report rule).
pub fn find_in_text(
    text: &str,
    term: &str,
    start: usize,
    forward: bool,
    wrap: bool,
) -> Option<(usize, usize)> {
    if text.is_empty() || term.is_empty() {
        return None;
    }
    let hay = lower_chars(text);
    let needle = lower_chars(term);
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let start = start.min(hay.len());
    let idx = if forward {
        find_from(&hay, &needle, start).or_else(|| {
            if wrap {
                find_from(&hay, &needle, 0)
            } else {
                None
            }
        })
    } else {
        rfind_within(&hay, &needle, start)
            .or_else(|| if wrap { rfind_all(&hay, &needle) } else { None })
    };
    idx.map(|i| (i, i + needle.len()))
}

/// The help viewer's search (Python `HelpWindow._find`) over
/// already-lowercased text with an already-lowercased term:
/// forward starts at `caret + 1`, backward searches the text
/// before the caret, and both wrap once. Returns the match
/// index and whether the search wrapped, so the viewer can
/// announce the wrap. `None` means not found anywhere.
pub fn find_in_guide(
    search_text: &str,
    term: &str,
    caret: usize,
    forward: bool,
) -> Option<(usize, bool)> {
    if search_text.is_empty() || term.is_empty() {
        return None;
    }
    let hay: Vec<char> = search_text.chars().collect();
    let needle: Vec<char> = term.chars().collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let caret = caret.min(hay.len());
    if forward {
        match find_from(&hay, &needle, caret + 1) {
            Some(i) => Some((i, false)),
            None => find_from(&hay, &needle, 0).map(|i| (i, true)),
        }
    } else {
        match rfind_within(&hay, &needle, caret) {
            Some(i) => Some((i, false)),
            None => rfind_all(&hay, &needle).map(|i| (i, true)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_forward_from_start_and_wrap() {
        let text = "alpha beta alpha";
        assert_eq!(find_in_text(text, "alpha", 0, true, true), Some((0, 5)));
        assert_eq!(find_in_text(text, "alpha", 1, true, true), Some((11, 16)));
        // Past the last match: wraps with wrap=true...
        assert_eq!(find_in_text(text, "alpha", 12, true, true), Some((0, 5)));
        // ...and reports none with wrap=false (F3 terminal rule).
        assert_eq!(find_in_text(text, "alpha", 12, true, false), None);
    }

    #[test]
    fn reader_backward_ends_at_or_before_caret() {
        let text = "aaa";
        // rfind("aa", 0, 2) in Python is 0: the match at 1 would
        // end past the caret.
        assert_eq!(find_in_text(text, "aa", 2, false, false), Some((0, 2)));
        let text = "alpha beta alpha";
        assert_eq!(
            find_in_text(text, "alpha", 16, false, false),
            Some((11, 16))
        );
        assert_eq!(find_in_text(text, "alpha", 11, false, false), Some((0, 5)));
        assert_eq!(find_in_text(text, "alpha", 0, false, false), None);
        assert_eq!(find_in_text(text, "alpha", 0, false, true), Some((11, 16)));
    }

    #[test]
    fn case_insensitive_and_empty() {
        assert_eq!(find_in_text("AlPhA", "alpha", 0, true, false), Some((0, 5)));
        assert_eq!(find_in_text("alpha", "", 0, true, true), None);
        assert_eq!(find_in_text("", "a", 0, true, true), None);
        assert_eq!(find_in_text("ab", "abc", 0, true, true), None);
    }

    #[test]
    fn guide_find_wrap_flags() {
        let text = "one two one";
        // Forward from caret 0 starts at 1: the "one" at 0 is
        // only reachable by wrapping.
        assert_eq!(find_in_guide(text, "one", 0, true), Some((8, false)));
        assert_eq!(find_in_guide(text, "one", 8, true), Some((0, true)));
        // Backward from caret 8: match ending at 3.
        assert_eq!(find_in_guide(text, "one", 8, false), Some((0, false)));
        assert_eq!(find_in_guide(text, "one", 0, false), Some((8, true)));
        assert_eq!(find_in_guide(text, "three", 0, true), None);
    }
}
