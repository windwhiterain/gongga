//! Where the cursor stands inside the line's JSON object.
//!
//! See `docs/notes/client-complete.md`.
//!
//! # Invariant
//!
//! A place, never a parse: the text under the cursor is half typed by
//! definition, so it is first closed into a whole document ([`close`]), and the
//! grammar after that is `jsonc_parser`'s — strings, escapes, nesting and lists
//! are its business.
//!
//! # Invariant
//!
//! [`place`] answers `None` for text it cannot read as a document, and the
//! caller reads that as "offer nothing". A cursor past the end is clamped, never
//! refused: a front-end that redrew in the same tick sends a stale offset.

use jsonc_parser::common::Ranged;
use jsonc_parser::tokens::{Token, TokenAndRange};
use jsonc_parser::{CollectOptions, CommentCollectionStrategy, ParseOptions};

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
pub struct Place {
    /// Byte offsets into the line: the token a candidate replaces.
    ///
    /// # Invariant
    ///
    /// `end` never runs past the line's own end: it stops at the token's own
    /// end, never into what [`close`] appended.
    pub start: usize,
    pub end: usize,
    /// The field names from the root to the object the cursor is in; with a
    /// value, the last one is the field being filled.
    pub path: Vec<String>,
    pub typing: Typing,
    /// What has been typed of the token, with no quote around it.
    pub fragment: String,
    /// Whether the token has both of its quotes in the line already.
    pub quoted: bool,
    /// The fields of the enclosing object that are already written.
    pub fields: Vec<(String, Read)>,
    /// Every object the cursor stands in, outermost first: a property's schema
    /// may be chosen by a sibling of an outer object.
    pub levels: Vec<Vec<(String, Read)>>,
    /// How many objects enclose the token, its own included.
    pub open: usize,
    /// How many closers already stand after it, so what a candidate owes is
    /// `open - after`.
    pub after: usize,
}

/// What an object or a list being read waits for next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Key,
    /// A key is read; its colon is next.
    Colon,
    Value,
    /// A member is finished; a comma or the closer is next.
    Done,
}

/// One object being read.
#[derive(Debug)]
struct Frame {
    /// The field this object is the value of; `None` at the root.
    key: Option<String>,
    /// The field name being read, until its value is finished.
    field: Option<String>,
    /// The token range of the field name just read, for a place after it.
    key_token: Option<(usize, usize)>,
    /// The members finished so far, in the order the line wrote them.
    members: Vec<(String, Read)>,
    expect: Expect,
}

enum Container {
    Object(Frame),
    /// A list is part of no shape this completes; all that is kept is whether
    /// the cursor stands inside one.
    List,
}

/// What a text still owes before it reads as one document.
struct Owed {
    frames: Vec<(char, Expect)>,
    /// A quote has to be written before the closers.
    quote: bool,
    bare: Vec<(usize, usize)>,
}

/// A text that reads as a whole document, and what had to be written for it.
pub struct Closed {
    pub text: String,
    /// The line's bare tokens, by range.
    pub bare: Vec<(usize, usize)>,
}

/// The cursor's place in `text`, or `None` when nothing here is completed.
pub fn place(text: &str, cursor: usize) -> Option<Place> {
    let at = floor(text, cursor);
    let line = text.len();
    let closed = close(text);
    let tokens = match read(&closed.text) {
        Some(tokens) => tokens,
        None => read(&substituted(text, &closed)?)?,
    };
    let mut stack: Vec<Container> = Vec::new();
    let mut found: Option<Place> = None;
    for token in &tokens {
        let (start, end) = (token.start, token.end);
        if at < start {
            found = between(text, at, &stack);
            break;
        }
        if at < end || (at == end && bare_at(&closed.bare, start, end)) {
            found = inside(text, at, line, &stack, &token.kind, start, end);
            break;
        }
        step(&mut stack, &token.kind, start, end);
    }
    let mut place = match found {
        Some(place) => place,
        None => between(text, at, &stack)?,
    };
    for (from, to) in &closed.bare {
        if place.start == *from {
            // The document read a stand-in here; this is what the line states.
            place.end = *to;
            place.fragment = slice(text, *from, at);
            place.quoted = false;
        }
    }
    place.open = stack
        .iter()
        .filter(|one| matches!(one, Container::Object(_)))
        .count();
    place.after = tokens
        .iter()
        .filter(|token| {
            matches!(token.kind, PieceKind::CloseBrace)
                && token.start >= place.end
                && token.start < line
        })
        .count()
        .min(place.open);
    Some(place)
}

/// The text with what it still owes appended, so a whole document can be read.
///
/// # Invariant
///
/// Only the end is written: every byte of `text` keeps its offset, so a range
/// read off the closed document is a range in the line.
pub fn close(text: &str) -> Closed {
    let owed = scan(text);
    Closed {
        text: whole(text, &owed),
        bare: owed.bare,
    }
}

/// `text` as a document whose bare tokens stand as values of their own, so the
/// place among them can be read.
///
/// # Invariant
///
/// A stand-in is exactly as long as the token it replaces, so every byte keeps
/// its offset and the tokens read here are the line's tokens.
fn substituted(text: &str, closed: &Closed) -> Option<String> {
    if closed.bare.is_empty() {
        return None;
    }
    let mut body = text.to_string();
    for (from, to) in &closed.bare {
        body.replace_range(*from..*to, &stand_in(to.saturating_sub(*from)));
    }
    let owed = scan(&body);
    Some(whole(&body, &owed))
}

/// A JSON value stated in as many bytes as `width`, standing where a word was.
fn stand_in(width: usize) -> String {
    match width {
        0 => String::new(),
        1 => "0".to_string(),
        2 => "\"\"".to_string(),
        _ => format!("\"{}\"", "x".repeat(width - 2)),
    }
}

/// One token of the document, owned: what it is, and where it stands.
///
/// # Invariant
///
/// `start` and `end` are offsets into the document that was read, and every
/// document read here keeps the line's own bytes at their own offsets.
struct Piece {
    kind: PieceKind,
    start: usize,
    end: usize,
}

enum PieceKind {
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Comma,
    Colon,
    Text(String),
    /// A number, a word or a literal the line stated.
    Bare(String),
    Null,
    Comment,
}

impl Piece {
    fn of(token: &TokenAndRange<'_>) -> Self {
        let kind = match &token.token {
            Token::OpenBrace => PieceKind::OpenBrace,
            Token::CloseBrace => PieceKind::CloseBrace,
            Token::OpenBracket => PieceKind::OpenBracket,
            Token::CloseBracket => PieceKind::CloseBracket,
            Token::Comma => PieceKind::Comma,
            Token::Colon => PieceKind::Colon,
            Token::String(text) => PieceKind::Text(text.to_string()),
            Token::Word(text) => PieceKind::Bare(text.to_string()),
            Token::Number(text) => PieceKind::Bare(text.to_string()),
            Token::Boolean(value) => PieceKind::Bare(value.to_string()),
            Token::Null => PieceKind::Null,
            Token::CommentLine(_) | Token::CommentBlock(_) => PieceKind::Comment,
        };
        Piece {
            kind,
            start: token.start(),
            end: token.end(),
        }
    }
}

/// Reads the tokens of a whole document, or `None` when it is not one.
fn read(text: &str) -> Option<Vec<Piece>> {
    let parsed = jsonc_parser::parse_to_ast(
        text,
        &CollectOptions {
            comments: CommentCollectionStrategy::Off,
            tokens: true,
        },
        &options(),
    )
    .ok()?;
    parsed
        .tokens
        .map(|tokens| tokens.iter().map(Piece::of).collect())
}

/// The options a document is read under.
///
/// # Invariant
///
/// Loose only where a half-typed line is ordinary: a trailing comma or a missing
/// one is a place every keystroke passes through. Nothing looser, so what this
/// reads is what the server would read.
fn options() -> ParseOptions {
    ParseOptions {
        allow_comments: false,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: true,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// What `text` still owes; the caller writes it with [`whole`].
fn scan(text: &str) -> Owed {
    let mut frames: Vec<(char, Expect)> = Vec::new();
    let mut bare_ranges: Vec<(usize, usize)> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut bare: Option<usize> = None;
    for (at, c) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                string_ended(&mut frames);
            }
            continue;
        }
        if let Some(from) = bare {
            if !(c.is_whitespace() || matches!(c, ',' | '}' | ']')) {
                continue;
            }
            bare = None;
            bare_ranges.push((from, at));
            finished(&mut frames);
        }
        match c {
            '"' => {
                in_string = true;
                escaped = false;
            }
            '{' => {
                opened(&mut frames);
                frames.push(('}', Expect::Key));
            }
            '[' => {
                opened(&mut frames);
                frames.push((']', Expect::Value));
            }
            '}' | ']' => {
                frames.pop();
                finished(&mut frames);
            }
            ':' => {
                if let Some(top) = frames.last_mut() {
                    top.1 = Expect::Value;
                }
            }
            ',' => {
                if let Some(top) = frames.last_mut() {
                    top.1 = match top.0 {
                        '}' => Expect::Key,
                        _ => Expect::Value,
                    };
                }
            }
            _ if c.is_whitespace() => {}
            _ => bare = Some(at),
        }
    }
    if in_string {
        string_ended(&mut frames);
    }
    if let Some(from) = bare {
        bare_ranges.push((from, text.len()));
        finished(&mut frames);
    }
    Owed {
        frames,
        quote: in_string,
        bare: bare_ranges,
    }
}

/// `body` with what `owed` still owes written after it.
fn whole(body: &str, owed: &Owed) -> String {
    let mut out = String::with_capacity(body.len() + 8);
    out.push_str(body);
    if owed.quote {
        out.push('"');
    }
    for (closer, expect) in owed.frames.iter().rev() {
        match expect {
            Expect::Value => out.push_str("null"),
            Expect::Colon => out.push_str(":null"),
            _ => {}
        }
        out.push(*closer);
    }
    out
}

/// A string ended: a field name if its object was waiting for one, else a value.
fn string_ended(frames: &mut [(char, Expect)]) {
    match frames.last_mut() {
        Some(top) if top.0 == '}' && top.1 == Expect::Key => top.1 = Expect::Colon,
        _ => finished(frames),
    }
}

/// A value opened: the frame it belongs to is satisfied by it.
fn opened(frames: &mut [(char, Expect)]) {
    finished(frames);
}

/// A value ended: the frame holding it is between members.
fn finished(frames: &mut [(char, Expect)]) {
    if let Some(top) = frames.last_mut() {
        top.1 = Expect::Done;
    }
}

/// Reads one token that stands before the cursor.
fn step(stack: &mut Vec<Container>, token: &PieceKind, start: usize, end: usize) {
    match token {
        PieceKind::OpenBrace => {
            let key = take_field(stack);
            stack.push(Container::Object(Frame {
                key,
                field: None,
                key_token: None,
                members: Vec::new(),
                expect: Expect::Key,
            }));
        }
        PieceKind::CloseBrace => {
            if let Some(Container::Object(done)) = stack.pop() {
                finish(stack, Read::Object(done.members));
            }
        }
        PieceKind::OpenBracket => {
            take_field(stack);
            stack.push(Container::List);
        }
        PieceKind::CloseBracket => {
            stack.pop();
            finish(stack, Read::Text(String::new()));
        }
        PieceKind::Colon => {
            if let Some(frame) = object_mut(stack) {
                frame.expect = Expect::Value;
            }
        }
        PieceKind::Comma => {
            if let Some(frame) = object_mut(stack) {
                frame.field = None;
                frame.key_token = None;
                frame.expect = Expect::Key;
            }
        }
        PieceKind::Text(text) => {
            let named = match object_mut(stack) {
                Some(frame) if frame.expect == Expect::Key => {
                    frame.field = Some(text.clone());
                    frame.key_token = Some((start, end));
                    frame.expect = Expect::Colon;
                    true
                }
                _ => false,
            };
            if !named {
                finish(stack, Read::Text(text.clone()));
            }
        }
        PieceKind::Bare(text) => finish(stack, Read::Text(text.clone())),
        PieceKind::Null => finish(stack, Read::Text("null".to_string())),
        PieceKind::Comment => {}
    }
}

/// The place in the gap before a token: nothing is being typed there.
fn between(text: &str, at: usize, stack: &[Container]) -> Option<Place> {
    let frame = object(stack)?;
    let typing = match frame.expect {
        Expect::Key | Expect::Colon => Typing::Key,
        Expect::Value => Typing::Value,
        Expect::Done => return None,
    };
    // A name already read takes the colon that follows it with it.
    let (start, end, fragment, quoted) = match (typing, frame.expect) {
        (Typing::Key, Expect::Colon) => {
            let (start, end) = frame.key_token?;
            let fragment = inner(text, start, end);
            let end = match text.get(end.min(text.len())..) {
                Some(rest) if rest.starts_with(':') => end + 1,
                _ => end,
            };
            (start, end, fragment, true)
        }
        _ => (at, at, String::new(), false),
    };
    Some(Place {
        start,
        end,
        path: path_of(stack, typing, frame),
        typing,
        fragment,
        quoted,
        fields: frame.members.clone(),
        levels: levels(stack),
        open: 0,
        after: 0,
    })
}

/// The place on the token the cursor stands in.
fn inside(
    text: &str,
    at: usize,
    line: usize,
    stack: &[Container],
    token: &PieceKind,
    start: usize,
    end: usize,
) -> Option<Place> {
    let frame = object(stack)?;
    let (typing, fragment) = match token {
        PieceKind::Text(_) => {
            let typing = match frame.expect {
                Expect::Key | Expect::Colon => Typing::Key,
                _ => Typing::Value,
            };
            // The token's start is the quote, which is not part of what was typed.
            (typing, slice(text, start.saturating_add(1), at))
        }
        PieceKind::Bare(_) | PieceKind::Null => (Typing::Value, slice(text, start, at)),
        _ => return None,
    };
    Some(Place {
        start,
        end: end.min(line),
        path: path_of(stack, typing, frame),
        typing,
        fragment,
        quoted: end <= line,
        fields: frame.members.clone(),
        levels: levels(stack),
        open: 0,
        after: 0,
    })
}

/// Every object's stated members, outermost first.
fn levels(stack: &[Container]) -> Vec<Vec<(String, Read)>> {
    stack
        .iter()
        .filter_map(|one| match one {
            Container::Object(frame) => Some(frame.members.clone()),
            Container::List => None,
        })
        .collect()
}

/// The path the place names: the objects it stands in, and the field it fills.
fn path_of(stack: &[Container], typing: Typing, frame: &Frame) -> Vec<String> {
    let mut out: Vec<String> = stack
        .iter()
        .filter_map(|one| match one {
            Container::Object(frame) => frame.key.clone(),
            Container::List => None,
        })
        .collect();
    if typing == Typing::Value {
        if let Some(field) = &frame.field {
            out.push(field.clone());
        }
    }
    out
}

fn object(stack: &[Container]) -> Option<&Frame> {
    match stack.last() {
        Some(Container::Object(frame)) => Some(frame),
        _ => None,
    }
}

fn object_mut(stack: &mut [Container]) -> Option<&mut Frame> {
    match stack.last_mut() {
        Some(Container::Object(frame)) => Some(frame),
        _ => None,
    }
}

/// Takes the field name waiting for the value that just opened.
fn take_field(stack: &mut [Container]) -> Option<String> {
    object_mut(stack).and_then(|frame| frame.field.take())
}

/// Records the value just read under the name that waited for it.
fn finish(stack: &mut [Container], value: Read) {
    if let Some(frame) = object_mut(stack) {
        if let Some(field) = frame.field.take() {
            frame.members.push((field, value));
        }
        frame.expect = Expect::Done;
        frame.key_token = None;
    }
}

/// Whether the cursor stands at the end of a token the line typed bare.
///
/// # Invariant
///
/// A word a value is being typed as is still that value at its own end: `t` is
/// where `true` is read from, not where a finished member is.
fn bare_at(bare: &[(usize, usize)], start: usize, end: usize) -> bool {
    bare.iter().any(|(from, to)| *from == start && *to == end)
}

/// The text between a token's quotes.
fn inner(text: &str, start: usize, end: usize) -> String {
    let from = floor(text, start.saturating_add(1));
    let to = floor(text, end.saturating_sub(1).max(from));
    slice(text, from, to)
}

/// A slice clamped to the line: half-typed JSON has no shape to trust, and a
/// completion is not a place to fail.
fn slice(text: &str, from: usize, to: usize) -> String {
    let from = floor(text, from);
    let to = floor(text, to.max(from));
    text[from..to].to_string()
}

/// `at` moved back to the nearest character boundary, and into the text.
fn floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}
