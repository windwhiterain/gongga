//! The completion vocabulary shared with the front-end.
//!
//! See `docs/notes/cli-complete.md`.

use serde::{Deserialize, Serialize};

/// One completion option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// What one completion is: the options, and what they are for.
///
/// # Invariant
///
/// `subject` names the argument whose value these complete (`--cache_tier`), and
/// is `None` when they are a command name, a flag name or a reference: the popup
/// says what it is asking for, so a value is never read as some other list.
#[derive(Debug, Clone, Default)]
pub struct Offered {
    pub subject: Option<String>,
    pub candidates: Vec<Candidate>,
    /// Where the range begins, when it is not the token's start: a reference sits
    /// inside a word (`看下@file(x)`).
    pub start: Option<usize>,
    /// Where the range ends, when not the token's end: a JSON value ends at its
    /// own quotes.
    pub end: Option<usize>,
}

/// The open completion popup: which token is replaced, and the options.
/// See `docs/notes/cli-complete.md`.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Byte range of the token being replaced.
    pub start: usize,
    pub end: usize,
    /// The argument being completed, for the popup's title.
    pub subject: Option<String>,
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
    fn complete(&self, line: &str, cursor: usize) -> Offered;

    /// Whether the last completion asked for something only the connection can
    /// answer.
    ///
    /// # Invariant
    ///
    /// `complete` stays synchronous, so a value it cannot answer now is asked for
    /// later and reported here. A front-end reads this after a key it handled and
    /// goes round its loop, which is the one place an ask can be sent from.
    fn wants(&self) -> bool {
        false
    }
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
