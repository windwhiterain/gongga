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

/// A single-line editor with a byte cursor.
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
        let prev = self
            .text[..self.cursor]
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

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn word_left(&mut self) {
        let before = self.text[..self.cursor].trim_end();
        self.cursor = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
    }
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
            candidates: vec![
                Candidate::word("a", ""),
                Candidate::word("b", ""),
            ],
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
                candidates: vec![Candidate { insert: "src/".into(), detail: "dir".into(), space: false }],
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
}
