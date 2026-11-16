//! The completion vocabulary shared with the front-end.
//!
//! See `docs/notes/client-complete.md`.

use serde::{Deserialize, Serialize};

/// One completion option: what the popup shows, and the text it writes.
///
/// # Invariant
///
/// The popup draws `label` and `detail`, and accepting the option replaces the
/// answer's range with `new_text` — or with this item's own [`Item::range`], when
/// it states one. `snippet` says whether `new_text` carries stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Shown in the popup.
    pub label: String,
    /// Shown on the right of the popup.
    pub detail: String,
    /// What accepting it writes.
    pub new_text: String,
    /// Whether `new_text` carries snippet stops (`$0`), expanded by
    /// [`expand`] where it is applied.
    #[serde(default)]
    pub snippet: bool,
    /// The bytes this item replaces, when the answer's own range is not it.
    ///
    /// # Invariant
    ///
    /// An answer states one range for all of its items; an item states its own
    /// only where its token is not that range. See `Offered`.
    #[serde(default)]
    pub range: Option<(usize, usize)>,
}

impl Item {
    /// A word, with a space after it.
    pub fn word(insert: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::plain(insert, detail, true)
    }

    /// A word nothing follows: a directory a key's descent enters.
    pub fn path(insert: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::plain(insert, detail, false)
    }

    /// A word as it is written, with a space after it only when `space` says so.
    fn plain(insert: impl Into<String>, detail: impl Into<String>, space: bool) -> Self {
        let text: String = insert.into();
        let mut new_text = text.clone();
        if space {
            new_text.push(' ');
        }
        Self {
            label: text,
            detail: detail.into(),
            new_text,
            snippet: false,
            range: None,
        }
    }

    /// An item that writes a whole token by itself, over its own range.
    ///
    /// # Invariant
    ///
    /// `new_text` is what the line holds after accepting it, minus snippet
    /// expansion: a token that opens something writes its closers too, and the
    /// caret lands on the stop `$0` when `snippet` is set.
    pub fn at(
        range: (usize, usize),
        new_text: impl Into<String>,
        snippet: bool,
        label: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            new_text: new_text.into(),
            snippet,
            range: Some(range),
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
///
/// `start` and `end` are the range every item replaces unless it states its own
/// ([`Item::range`]). They are `None` for the token under the cursor.
#[derive(Debug, Clone, Default)]
pub struct Offered {
    pub subject: Option<String>,
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub items: Vec<Item>,
}

/// The open completion popup: which token is replaced, and the options.
///
/// See `docs/notes/client-complete.md`.
#[derive(Debug, Clone)]
pub struct Completion {
    /// The line this answers, and the caret it was asked at.
    ///
    /// # Invariant
    ///
    /// An answer belongs to the text it was computed from, and says which: a
    /// window applies it only while its own input still reads this, so a key typed
    /// between the question and the answer is never edited by an answer for the
    /// words it replaced. See `docs/notes/tui-lib.md`.
    pub line: String,
    pub cursor: usize,
    /// Byte range of the token being replaced, for the items that state none.
    pub start: usize,
    pub end: usize,
    /// The argument being completed, for the popup's title.
    pub subject: Option<String>,
    pub items: Vec<Item>,
    pub selected: usize,
}

impl Completion {
    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let current = self.selected as isize;
        self.selected = (((current + delta) % len + len) % len) as usize;
    }

    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    /// What accepting `item` writes, and where the caret lands in the line.
    ///
    /// # Invariant
    ///
    /// The edit is applied whole: the item's own range or the token's, never a
    /// part of either. A word never doubles the space it ends with.
    pub fn apply(&self, item: &Item) -> Option<(usize, usize, String, usize)> {
        let (start, end) = item.range.unwrap_or((self.start, self.end));
        if start > end || end > self.line.len() {
            return None;
        }
        if !self.line.is_char_boundary(start) || !self.line.is_char_boundary(end) {
            return None;
        }
        let (mut new_text, caret) = match item.snippet {
            true => expand(&item.new_text),
            false => (item.new_text.clone(), None),
        };
        // A word never doubles the space it ends with: the line already has one.
        if new_text.ends_with(' ') && self.line[end..].starts_with(char::is_whitespace) {
            new_text.pop();
        }
        if new_text.is_empty() && start == end {
            return None;
        }
        let at = caret.unwrap_or(new_text.len()).min(new_text.len());
        Some((start, end, new_text, start + at))
    }
}

/// The text a snippet writes, and where the caret lands in it.
///
/// # Invariant
///
/// A stop is `$0`..`$9` or `${0}`..`${9}`; `${n:word}` writes `word` and is the
/// stop too. The caret lands on the lowest stop, at its first appearance, and a
/// snippet stating no stop leaves it at the end. Nothing else is a stop: a `$`
/// that names none is text, since a value may hold one.
pub fn expand(snippet: &str) -> (String, Option<usize>) {
    let mut out = String::with_capacity(snippet.len());
    let mut stop: Option<(u8, usize)> = None;
    let bytes = snippet.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        let digit = match bytes[at] {
            b'$' => match bytes.get(at + 1) {
                Some(d) if d.is_ascii_digit() => Some((*d, at + 2, None)),
                Some(b'{') => braced_stop(bytes, at),
                _ => None,
            },
            _ => None,
        };
        let Some((number, after, default)) = digit else {
            let ch = snippet[at..].chars().next().unwrap_or('\0');
            out.push(ch);
            at += ch.len_utf8();
            continue;
        };
        let number = number - b'0';
        // The lowest stop decides, and the first of that number keeps the place.
        match stop {
            Some((lowest, _)) if lowest <= number => {}
            _ => stop = Some((number, out.len())),
        }
        if let Some((from, to)) = default {
            out.push_str(&snippet[from..to]);
        }
        at = after;
    }
    (out, stop.map(|(_, at)| at))
}

/// A `${n}` or `${n:word}` stop beginning at `from`: the stop, where it ends, and
/// the default text it writes.
fn braced_stop(bytes: &[u8], from: usize) -> Option<(u8, usize, Option<(usize, usize)>)> {
    let number = *bytes.get(from + 2)?;
    if !number.is_ascii_digit() {
        return None;
    }
    match bytes.get(from + 3)? {
        b'}' => Some((number, from + 4, None)),
        b':' => {
            let end = bytes[from + 4..].iter().position(|b| *b == b'}')? + from + 4;
            Some((number, end + 1, Some((from + 4, end))))
        }
        _ => None,
    }
}

/// Supplies completion options for the token under the cursor; empty means offer
/// nothing.
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
/// See `docs/notes/client-complete.md`.
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
