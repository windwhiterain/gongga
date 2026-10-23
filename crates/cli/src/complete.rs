//! The completion vocabulary shared with the front-end.
//!
//! See `docs/notes/cli-complete.md`.

/// One completion option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Text that replaces the token being completed.
    pub insert: String,
    /// Shown on the right of the popup.
    pub detail: String,
    /// Whether a space follows the insertion; false for a directory, so Tab
    /// descends into it.
    pub space: bool,
}

impl Candidate {
    /// The ordinary case: a word, with a space after it.
    pub fn word(insert: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            insert: insert.into(),
            detail: detail.into(),
            space: true,
        }
    }
}

/// The open completion popup: which token is replaced, and the options.
/// See `docs/notes/cli-complete.md`.
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

/// Supplies completion candidates for the token under the cursor; empty means
/// offer nothing.
pub trait Completer: Send + Sync {
    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate>;
}

/// A completer the front-end shares; the command layer swaps the catalog in it.
pub type SharedCompleter = std::sync::Arc<dyn Completer + Send + Sync>;

/// Byte range of the whitespace-delimited token containing `cursor`.
///
/// See `docs/notes/cli-complete.md`.
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
