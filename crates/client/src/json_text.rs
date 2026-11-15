//! One JSON text, read two ways: as written, and as a person typed it.
//!
//! See `docs/notes/client-complete.md`.
//!
//! # Invariant
//!
//! A submission binds real values, so it must not guess; a line arriving from a
//! keyboard is another matter. Both readers use this one module, and only the
//! second forgives: a text nothing forgives is refused with the parser's own
//! reason, never silently rewritten into something else.

use serde_json::Value;

/// `text` as JSON, forgiving only what a person's typing produces.
///
/// # Invariant
///
/// As written first, so a text that parses is never rewritten; then a trailing
/// comma before a closer is dropped, outside strings, and the result is kept only
/// if it parses. A text no rule fixes is `None`, and the caller says why.
pub fn parse_lenient(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    let repaired = drop_trailing_commas(text);
    if repaired != text
        && let Ok(value) = serde_json::from_str::<Value>(&repaired)
    {
        return Some(value);
    }
    None
}

/// `text` with a comma that nothing follows removed, outside strings only.
///
/// # Invariant
///
/// A comma inside a string is part of the string, and a comma that a value
/// follows is the separator it looks like: only the one whose next character is a
/// closer goes.
fn drop_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' && matches!(chars.get(i + 1), Some('}' | ']')) {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}
