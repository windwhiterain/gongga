//! The input line: editing primitives plus the completion popup.
//!
//! Nothing here knows what a command is. The caller hands this module an
//! arbitrary list of candidates for the token under the cursor and it renders
//! the popup; deciding *what* completes is the command layer's job, and both
//! the candidates and the token split come from there.
//!
//! What stays here is the part that is genuinely the editor's: what a keystroke
//! does to the text, and how a chosen candidate is spliced back in.

use crate::Completion;

/// A text editor with a byte cursor.
///
/// Multi-line, because a message to the agent is prose: a paragraph break is
/// part of what is being said, not a second message. The breaks live in the
/// text itself (`'\n'`), so everything that already treats the text as an
/// opaque string -- history, submission, the wire -- keeps working unchanged.
#[derive(Debug, Clone, Default)]
pub struct InputLine {
    text: String,
    cursor: usize,
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

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
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
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn kill_to_end(&mut self) {
        self.text.truncate(self.cursor);
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    /// Byte range of the logical line the cursor is on, newline excluded.
    ///
    /// Logical lines, not the rows the terminal wrapped them into: a break the
    /// operator typed is a place they can navigate to, and a wrap is only where
    /// the glass ended.
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
        self.cursor = self.line_bounds().0;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_bounds().1;
    }

    /// End of the whole text, not of the line: where a restored line and a
    /// recalled history entry want the cursor.
    pub fn to_document_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Moves to the line above, keeping the column. False when there is no line
    /// above, which is what lets the caller fall back to history.
    pub fn up(&mut self) -> bool {
        let (start, _) = self.line_bounds();
        if start == 0 {
            return false;
        }
        let column = self.cursor - start;
        let previous_end = start - 1; // the newline that ends the line above
        let previous_start = self.text[..previous_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.cursor = boundary(&self.text, (previous_start + column).min(previous_end));
        true
    }

    /// The line below, keeping the column. False on the last line.
    pub fn down(&mut self) -> bool {
        let (start, end) = self.line_bounds();
        if end >= self.text.len() {
            return false;
        }
        let column = self.cursor - start;
        let next_start = end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map(|i| next_start + i)
            .unwrap_or(self.text.len());
        self.cursor = boundary(&self.text, (next_start + column).min(next_end));
        true
    }

    pub fn word_left(&mut self) {
        let before = self.text[..self.cursor].trim_end();
        self.cursor = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
    }
}

/// `at`, moved back to the nearest char boundary.
///
/// Vertical moves keep a byte column, and a column that lands inside a wide
/// character has to round down rather than split it: slicing a `String` at a
/// non-boundary panics.
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
    line.text
        .replace_range(completion.start..completion.end, &candidate.insert);
    line.cursor = completion.start + candidate.insert.len();
    // Flags need a space before their value; command names before their flags.
    if candidate.space && !line.text[line.cursor..].starts_with(' ') {
        line.insert(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Candidate;

    fn completion() -> Completion {
        Completion {
            start: 0,
            end: 4,
            candidates: vec![Candidate::word("a", ""), Candidate::word("b", "")],
            selected: 0,
        }
    }

    #[test]
    fn selection_wraps() {
        let mut k = completion();
        k.move_selection(-1);
        assert_eq!(k.selected, 1);
        k.move_selection(1);
        assert_eq!(k.selected, 0);
    }

    #[test]
    fn applying_inserts_text_and_a_space() {
        let mut line = InputLine::default();
        line.insert_str("send");
        apply_completion(
            &mut line,
            &Completion {
                start: 0,
                end: 4,
                candidates: vec![Candidate::word("send_message", "")],
                selected: 0,
            },
        );
        assert_eq!(line.text(), "send_message ");
    }

    /// Tab on a directory has to leave the cursor against it, or the next Tab
    /// has nothing to descend into.
    #[test]
    fn applying_a_directory_leaves_no_space() {
        let mut line = InputLine::default();
        line.insert_str("src");
        apply_completion(
            &mut line,
            &Completion {
                start: 0,
                end: 3,
                candidates: vec![Candidate {
                    insert: "src/".into(),
                    detail: "dir".into(),
                    space: false,
                }],
                selected: 0,
            },
        );
        assert_eq!(line.text(), "src/");
        assert_eq!(line.cursor, 4);
    }

    #[test]
    fn editing_primitives() {
        let mut line = InputLine::default();
        line.insert_str("abc");
        line.left();
        line.backspace();
        assert_eq!(line.text(), "ac");
        line.home();
        line.insert('x');
        assert_eq!(line.text(), "xac");
        line.end();
        line.kill_to_end();
        assert_eq!(line.text(), "xac");
    }

    /// A message can have paragraphs, so the text has to survive a newline and
    /// the vertical moves have to walk them -- otherwise the second paragraph
    /// is reachable only by arrowing through the first one character at a time.
    #[test]
    fn a_newline_starts_a_line_the_cursor_can_leave() {
        let mut line = InputLine::default();
        line.insert_str("one");
        line.insert('\n');
        line.insert_str("two");
        assert_eq!(line.text(), "one\ntwo");

        // Up from the end of line two lands at the end of line one, not at 0.
        assert!(line.up());
        assert_eq!(line.cursor, 3);
        // ... and there is nothing above that.
        assert!(!line.up());
        assert!(line.down());
        assert_eq!(line.cursor, 7);

        // Home and End are line-scoped now, not text-scoped.
        line.home();
        assert_eq!(line.cursor, 4);
        line.end();
        assert_eq!(line.cursor, 7);
        // The document end is still reachable, which is what a restored line
        // and a recalled history entry need.
        line.to_document_end();
        assert_eq!(line.cursor, 7);
        line.home();
        line.kill_to_end();
        assert_eq!(line.text(), "one\n");
    }

    /// A column lands on a byte, and a byte can be inside a wide character.
    #[test]
    fn vertical_moves_round_down_to_a_char_boundary() {
        let mut line = InputLine::default();
        line.insert_str("中文\nab");
        // Column 2 on the second line; `中` is three bytes, so the same column
        // on the first line is inside it and has to round back to its start.
        line.up();
        assert_eq!(line.cursor, 0);
        assert!(line.text().is_char_boundary(line.cursor));
    }
}
