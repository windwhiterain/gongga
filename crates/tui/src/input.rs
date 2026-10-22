//! The input line: editing primitives plus the completion popup.
//!
//! Nothing here knows what a command is. The caller hands this module an
//! arbitrary list of candidates for the token under the cursor and it renders
//! the popup; deciding *what* completes is [`crate::Completer`]'s job, and that
//! lives outside this crate.

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

/// One completion option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Text that replaces the token being completed.
    pub insert: String,
    /// Shown on the right of the popup.
    pub detail: String,
}

/// An open completion popup.
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

/// Applies the selected candidate to the input line.
pub fn apply_completion(line: &mut InputLine, completion: &Completion) {
    let Some(candidate) = completion.current() else {
        return;
    };
    line.text
        .replace_range(completion.start..completion.end, &candidate.insert);
    line.cursor = completion.start + candidate.insert.len();
    // Flags need a space before their value; command names before their flags.
    if !line.text[line.cursor..].starts_with(' ') {
        line.insert(' ');
    }
}

/// Byte range of the whitespace-delimited token containing `cursor`.
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

    fn completion() -> Completion {
        Completion {
            start: 0,
            end: 4,
            candidates: vec![
                Candidate { insert: "a".into(), detail: "".into() },
                Candidate { insert: "b".into(), detail: "".into() },
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
                candidates: vec![Candidate { insert: "send_message".into(), detail: "".into() }],
                selected: 0,
            },
        );
        assert_eq!(line.text(), "send_message ");
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

    #[test]
    fn token_bounds_respects_utf8() {
        // Byte 8 sits inside the CJK run, so it must stay a boundary.
        let (s, e, t) = token_bounds("echo 中文 x", 8);
        assert_eq!(&"echo 中文 x"[s..e], t);
        assert_eq!(t, "中文");
    }
}
