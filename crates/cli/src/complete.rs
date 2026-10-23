//! The completion vocabulary shared with the front-end.
//!
//! These types live here, in the command layer, because completion is about
//! commands: the catalog decides what a token could become. The front-end
//! renders the popup and inserts the chosen text, but it supplies no knowledge
//! of its own -- which is what lets `tui` remain ignorant of commands while
//! still offering Tab completion.

/// One completion option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Text that replaces the token being completed.
    pub insert: String,
    /// Shown on the right of the popup.
    pub detail: String,
    /// Whether a space follows the insertion. False for a directory, so that
    /// Tab again descends into it instead of starting a new word.
    pub space: bool,
}

impl Candidate {
    /// The ordinary case: a word, with a space after it.
    pub fn word(insert: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { insert: insert.into(), detail: detail.into(), space: true }
    }
}

/// An open completion popup: which token is being replaced, and what the
/// options are.
///
/// The `selected` index lives here rather than in the front-end because it is
/// part of the same decision: a popup whose contents and whose current choice
/// were tracked separately could disagree, and the disagreement would show up
/// as the wrong text being inserted.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Byte range of the token being replaced.
    pub start: usize,
    pub end: usize,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
}

impl Completion {
    pub fn move_selection(&mut self, delta: isize) {
        if self.candidates.is_empty() {
            return;
        }
        let len = self.candidates.len() as isize;
        let current = self.selected as isize;
        self.selected = (((current + delta) % len + len) % len) as usize;
    }

    pub fn current(&self) -> Option<&Candidate> {
        self.candidates.get(self.selected)
    }
}

/// Supplies completion candidates for the token under the cursor.
///
/// An empty vector means "nothing to offer", which is how the popup stays
/// closed for free-form values.
pub trait Completer: Send + Sync {
    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate>;
}

/// A shared completer: the front-end holds it, the command layer swaps the
/// catalog inside it whenever the server publishes a new one.
pub type SharedCompleter = std::sync::Arc<dyn Completer + Send + Sync>;

/// Byte range of the whitespace-delimited token containing `cursor`.
///
/// Completion is decided by *which token* is under the cursor, and the
/// candidate source (the catalog) is here, so the split lives here too. The
/// front-end uses the same split when it applies a candidate, which is why this
/// is shared rather than duplicated: two answers to "where does this token
/// start" would drift, and the drift would show up as text inserted in the
/// wrong place.
pub fn token_bounds(input: &str, cursor: usize) -> (usize, usize, String) {
    let before = &input[..cursor.min(input.len())];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let after = &input[cursor.min(input.len())..];
    let end = after
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| cursor + i)
        .unwrap_or(input.len());
    let start = start.min(end);
    (start, end, input[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bounds_respects_utf8() {
        let (s, e, t) = token_bounds("echo 中文 x", 8);
        assert_eq!((s, e, t.as_str()), (5, 11, "中文"));
    }

    #[test]
    fn token_bounds_clamps_a_cursor_past_the_end() {
        let (s, e, t) = token_bounds("one", 99);
        assert_eq!((s, e, t.as_str()), (0, 3, "one"));
    }
}
