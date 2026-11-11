//! The input line: editing primitives, plus a popup someone else fills.

use crate::Completion;

/// A text editor with a byte cursor; multi-line, breaks being `'\n'` in the text.
#[derive(Debug, Clone, Default)]
pub struct InputLine {
    text: String,
    cursor: usize,
    /// Where a selection began: the end that stays put while the cursor is the
    /// end that moves.
    anchor: Option<usize>,
}

impl InputLine {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The bytes a selection covers, if there is one.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// The selected text, if there is a selection.
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection()?;
        self.text.get(start..end)
    }

    /// Puts the caret where a click landed, and ends any selection.
    pub fn set_cursor(&mut self, at: usize) {
        self.cursor = boundary(&self.text, at);
        self.anchor = None;
    }

    /// Selects from `anchor` to `at`, the way a drag does.
    pub fn select(&mut self, anchor: usize, at: usize) {
        self.anchor = Some(boundary(&self.text, anchor));
        self.cursor = boundary(&self.text, at);
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn insert(&mut self, ch: char) {
        self.delete_selection();
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map(char::len_utf8)
            .unwrap_or(1);
        self.cursor -= prev;
        self.text.remove(self.cursor);
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn kill_to_end(&mut self) {
        if self.delete_selection() {
            return;
        }
        self.text.truncate(self.cursor);
    }

    pub fn left(&mut self) {
        if self.collapse(false) {
            return;
        }
        if let Some(prev) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if self.collapse(true) {
            return;
        }
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    /// Byte range of the logical line the cursor is on; a wrap is not a line.
    fn line_bounds(&self) -> (usize, usize) {
        let start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
        (start, end)
    }

    pub fn home(&mut self) {
        self.anchor = None;
        self.cursor = self.line_bounds().0;
    }

    pub fn end(&mut self) {
        self.anchor = None;
        self.cursor = self.line_bounds().1;
    }

    /// End of the whole text, not of the line: where a restored draft leaves the cursor.
    pub fn to_document_end(&mut self) {
        self.anchor = None;
        self.cursor = self.text.len();
    }

    pub fn word_left(&mut self) {
        if self.collapse(false) {
            return;
        }
        let before = self.text[..self.cursor].trim_end();
        self.cursor = before
            .rfind(char::is_whitespace)
            .map(|i| i + before[i..].chars().next().unwrap().len_utf8())
            .unwrap_or(0);
    }

    /// Ends a selection by putting the caret at one of its ends.
    ///
    /// # Invariant
    ///
    /// The caret lands on the end the key names: the start for a move to the
    /// left, the end for a move to the right. One press therefore reaches the
    /// edge of what was selected instead of walking off it.
    fn collapse(&mut self, to_end: bool) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        self.cursor = if to_end { end } else { start };
        self.anchor = None;
        true
    }

    /// Deletes the selected bytes, if there are any; says whether it did.
    ///
    /// # Invariant
    ///
    /// The caret ends where the selection began, so an edit that replaces a
    /// selection leaves the text where the selection was, not where the caret
    /// happened to be.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.anchor = None;
        true
    }

    /// The end of the word the caret is in, or of the next one.
    ///
    /// # Invariant
    ///
    /// The other end of `word_left`, which lands on a word's beginning: the
    /// two keys put the caret at the two ends of a word.
    pub fn word_right(&mut self) {
        if self.collapse(true) {
            return;
        }
        let from = self.cursor;
        let Some(start) = self.text[from..]
            .char_indices()
            .find(|(_, ch)| !ch.is_whitespace())
            .map(|(index, _)| from + index)
        else {
            self.cursor = self.text.len();
            return;
        };
        self.cursor = self.text[start..]
            .find(char::is_whitespace)
            .map(|index| start + index)
            .unwrap_or(self.text.len());
    }
}

/// `at`, rounded down to a char boundary, since slicing off one panics.
fn boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Applies the selected candidate to the input line.
pub fn apply_completion(line: &mut InputLine, completion: &Completion) {
    let Some(candidate) = completion.current() else {
        return;
    };
    if completion.start > completion.end
        || completion.end > line.text.len()
        || !line.text.is_char_boundary(completion.start)
        || !line.text.is_char_boundary(completion.end)
    {
        return;
    }
    line.text
        .replace_range(completion.start..completion.end, &candidate.insert);
    // Inside what the candidate opened, when it says so. See `Candidate::cursor`.
    let at = candidate.cursor.unwrap_or(candidate.insert.len());
    line.cursor = completion.start + at.min(candidate.insert.len());
    let ended = at >= candidate.insert.len();
    // The completion is the text now: what was selected is gone with it.
    line.anchor = None;
    // Flags need a space before their value; command names before their flags.
    if ended && candidate.space && !line.text[line.cursor..].starts_with(' ') {
        line.insert(' ');
    }
}
