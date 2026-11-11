//! Where the cursor sits inside a partly typed JSON object.
//!
//! See `docs/notes/client-complete.md`.
//!
//! # Invariant
//!
//! A place, never a parse: the value under the cursor is half typed, so nothing
//! here is an error and no character is refused for making the text unfinished.
//! What this cannot place -- a list, a closed object, a value that was never
//! opened -- is `None`, which the caller reads as "offer nothing".

/// What a finished value reads as, for a sibling value that depends on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// A string, a number, a literal: its text, with no quote around it.
    Text(String),
    /// A nested object: its fields, in the order it stated them.
    Object(Vec<(String, Read)>),
}

/// Whether the cursor names a field or fills one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typing {
    Key,
    Value,
}

/// Where the cursor sits inside a partly typed JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    /// Byte offsets into the text: the token a candidate replaces.
    ///
    /// # Invariant
    ///
    /// `start` is the token's opening quote when it has one, and `end` is past
    /// its closing quote. A candidate therefore replaces the whole token: a
    /// fragment left behind would be the head of the value, and a quote left
    /// behind would close the insertion in the middle.
    pub start: usize,
    pub end: usize,
    /// The field names from the root to the object the cursor is in; with a
    /// value, the last one is the field being filled.
    pub path: Vec<String>,
    pub typing: Typing,
    /// What has been typed of the token, with no quote around it.
    pub fragment: String,
    /// The fields of the enclosing object that are already written, for the
    /// values that depend on a sibling.
    pub fields: Vec<(String, Read)>,
    /// How many objects enclose the token.
    pub open: usize,
    /// How many closers already stand after it, so what a candidate owes is
    /// `open - after`. See `Candidate::cursor`.
    ///
    /// # Invariant
    ///
    /// Counted from the token's end, outside strings only: an operator may have
    /// written the braces already, and writing them twice is not balanced.
    pub after: usize,
}

/// One object being read.
struct Open {
    /// The field this object is the value of; `None` at the root.
    key: Option<String>,
    /// Whether this object carries its key on the path.
    pushed: bool,
    state: State,
    fields: Vec<(String, Read)>,
    /// Where the field name being read began, and whether it was quoted.
    key_token: Option<usize>,
    key_quoted: bool,
    /// Where a bare value being read began.
    value_token: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// A field name is expected, or being typed.
    Key,
    /// A field name is read; `:` is next.
    Colon,
    /// The value of [`Open::key`] is expected, or being typed.
    Value,
    /// A value is read; `,` or `}` is next.
    Comma,
}

/// The cursor's place in `text`, or `None` when nothing here is completed.
///
/// # Invariant
///
/// A cursor past the end is clamped, never refused: a front-end that redrew in
/// the same tick sends a stale offset, and that is not a failure.
pub fn spot(text: &str, cursor: usize) -> Option<Spot> {
    let cursor = floor(text, cursor);
    let mut opens: Vec<Open> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut in_string = false;
    let mut value_string = false;
    let mut escaped = false;
    let mut quote_at = 0usize;
    let mut chars = text.char_indices();
    let mut placed = None;
    while let Some((at, c)) = chars.next() {
        if at >= cursor {
            placed = place(
                text,
                cursor,
                &opens,
                &path,
                in_string,
                value_string,
                quote_at,
            );
            break;
        }
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '"' => {
                    in_string = false;
                    if let Some(open) = opens.last_mut() {
                        let content = between(text, quote_at + 1, at).to_string();
                        if value_string {
                            finish(open, Read::Text(content));
                        } else {
                            open.key = Some(content);
                            open.key_token = Some(quote_at);
                            open.key_quoted = true;
                            open.state = State::Colon;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                escaped = false;
                quote_at = at;
                value_string = matches!(opens.last().map(|open| open.state), Some(State::Value));
            }
            '{' => {
                let key = opens.last().and_then(|open| open.key.clone());
                opens.push(Open {
                    key: key.clone(),
                    pushed: key.is_some(),
                    state: State::Key,
                    fields: Vec::new(),
                    key_token: None,
                    key_quoted: false,
                    value_token: None,
                });
                if let Some(key) = key {
                    path.push(key);
                }
            }
            '}' => {
                if let Some(open) = opens.last_mut() {
                    close_bare(open, text, at);
                }
                let Some(done) = opens.pop() else {
                    return None;
                };
                if done.pushed {
                    path.pop();
                }
                if let Some(open) = opens.last_mut() {
                    finish(open, Read::Object(done.fields));
                }
            }
            ':' => {
                if let Some(open) = opens.last_mut() {
                    open.state = State::Value;
                    open.value_token = None;
                }
            }
            ',' => {
                if let Some(open) = opens.last_mut() {
                    close_bare(open, text, at);
                    open.state = State::Key;
                    open.key = None;
                    open.key_token = None;
                    open.key_quoted = false;
                    open.value_token = None;
                }
            }
            '[' => {
                // A list is not part of any shape this completes: all that has to
                // be known is whether the cursor is inside one.
                if !skip_list(&mut chars, cursor) {
                    return None;
                }
                if let Some(open) = opens.last_mut() {
                    finish(open, Read::Text(String::new()));
                }
            }
            c if c.is_whitespace() => {}
            _ => {
                if let Some(open) = opens.last_mut() {
                    match open.state {
                        State::Value => {
                            if open.value_token.is_none() {
                                open.value_token = Some(at);
                            }
                        }
                        State::Key | State::Colon => {
                            if open.key_token.is_none() {
                                open.key_token = Some(at);
                            }
                        }
                        State::Comma => {}
                    }
                }
            }
        }
    }
    placed.or_else(|| {
        place(
            text,
            cursor,
            &opens,
            &path,
            in_string,
            value_string,
            quote_at,
        )
    })
}

/// Skips the list that has just opened; false when the cursor is inside it.
fn skip_list(chars: &mut std::str::CharIndices<'_>, cursor: usize) -> bool {
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for (at, c) in chars.by_ref() {
        if at >= cursor {
            return false;
        }
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The place the cursor holds, given what has been read so far.
fn place(
    text: &str,
    cursor: usize,
    opens: &[Open],
    path: &[String],
    in_string: bool,
    value_string: bool,
    quote_at: usize,
) -> Option<Spot> {
    let mut spot = place_token(text, cursor, opens, path, in_string, value_string, quote_at)?;
    // A key's own object is open too; a value's path ends with its field.
    spot.open = spot.path.len() + usize::from(spot.typing == Typing::Key);
    spot.after = closers(text, spot.end).min(spot.open);
    Some(spot)
}

/// How many `}` stand from `from` on, outside strings.
fn closers(text: &str, from: usize) -> usize {
    let from = floor(text, from);
    let mut count = 0;
    let mut in_string = false;
    let mut escaped = false;
    for c in text[from..].chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '}' => count += 1,
            _ => {}
        }
    }
    count
}

/// The token's place, before the two counts [`place`] adds.
fn place_token(
    text: &str,
    cursor: usize,
    opens: &[Open],
    path: &[String],
    in_string: bool,
    value_string: bool,
    quote_at: usize,
) -> Option<Spot> {
    let open = opens.last()?;
    let fields = open.fields.clone();
    if in_string {
        return Some(Spot {
            start: quote_at,
            end: quoted_end(text, quote_at),
            path: if value_string {
                with_field(path, open)
            } else {
                path.to_vec()
            },
            typing: if value_string {
                Typing::Value
            } else {
                Typing::Key
            },
            fragment: between(text, quote_at + 1, cursor).to_string(),
            fields,
            open: 0,
            after: 0,
        });
    }
    match open.state {
        State::Key => {
            let start = open.key_token.unwrap_or(cursor);
            Some(Spot {
                start,
                end: bare_end(text, cursor),
                path: path.to_vec(),
                typing: Typing::Key,
                fragment: between(text, start, cursor).to_string(),
                fields,
                open: 0,
                after: 0,
            })
        }
        // The name is read and its quote is closed: accepting it writes the
        // colon that follows.
        State::Colon => {
            let start = open.key_token.unwrap_or(cursor);
            let fragment = if open.key_quoted {
                between(text, start + 1, quoted_end(text, start).saturating_sub(1)).to_string()
            } else {
                between(text, start, cursor).to_string()
            };
            Some(Spot {
                start,
                end: quoted_end(text, start),
                path: path.to_vec(),
                typing: Typing::Key,
                fragment,
                fields,
                open: 0,
                after: 0,
            })
        }
        State::Value => {
            let start = open.value_token.unwrap_or(cursor);
            Some(Spot {
                start,
                end: bare_end(text, cursor),
                path: with_field(path, open),
                typing: Typing::Value,
                fragment: between(text, start, cursor).to_string(),
                fields,
                open: 0,
                after: 0,
            })
        }
        State::Comma => None,
    }
}

/// The path plus the field this object is the value of.
fn with_field(path: &[String], open: &Open) -> Vec<String> {
    let mut out = path.to_vec();
    if let Some(key) = &open.key {
        out.push(key.clone());
    }
    out
}

/// Records the value just read under the name that was waiting for it.
fn finish(open: &mut Open, value: Read) {
    if let Some(key) = open.key.take() {
        open.fields.push((key, value));
    }
    open.state = State::Comma;
    open.key_token = None;
    open.key_quoted = false;
    open.value_token = None;
}

/// Closes a bare value at the delimiter that ends it.
fn close_bare(open: &mut Open, text: &str, at: usize) {
    if open.state != State::Value {
        return;
    }
    if let Some(start) = open.value_token {
        let value = between(text, start, at).trim().to_string();
        finish(open, Read::Text(value));
    }
}

/// The offset just past the closing quote of the string opening at `quote`.
fn quoted_end(text: &str, quote: usize) -> usize {
    let from = floor(text, quote.saturating_add(1));
    let mut escaped = false;
    for (at, c) in text[from..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => return from + at + 1,
            _ => {}
        }
    }
    text.len()
}

/// The end of the bare token beginning at `from`: the space or delimiter that
/// ends it, or the end of the text.
fn bare_end(text: &str, from: usize) -> usize {
    let from = floor(text, from);
    for (at, c) in text[from..].char_indices() {
        if c.is_whitespace() || matches!(c, ',' | '}' | ']' | ':') {
            return from + at;
        }
    }
    text.len()
}

/// A slice clamped to the text: half-typed JSON has no shape to trust, and a
/// completion is not a place to fail.
fn between(text: &str, from: usize, to: usize) -> &str {
    let from = floor(text, from);
    let to = floor(text, to.max(from));
    &text[from..to]
}

/// `at` moved back to the nearest character boundary, and into the text.
fn floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}
