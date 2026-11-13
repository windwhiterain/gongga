//! The `/name{...}` command form: one JSON object carries every argument.
//!
//! See `docs/notes/client-complete.md`.
//!
//! # Invariant
//!
//! One grammar, read two ways: the submission parses the object strictly, while
//! the completion splits the same text tolerantly, so a half-typed line is a
//! place to offer candidates and never an error. The name and the object are
//! split here once, and both readers use that split, so the popup replaces exactly
//! the text the parser will bind.

use protocol::Args;
use serde_json::{Map, Value};

/// Splits the text after `/` into the command name and the argument text.
///
/// # Invariant
///
/// The name runs to the first `{` or whitespace; the tail is what follows with
/// leading whitespace removed. Tolerant by contract: an unfinished line still
/// splits, because the completion reads it on every keystroke.
pub fn split(rest: &str) -> (String, String) {
    let end = rest
        .find(|c: char| c == '{' || c.is_whitespace())
        .unwrap_or(rest.len());
    let name = rest[..end].to_string();
    let tail = rest[end..].trim_start().to_string();
    (name, tail)
}

/// Whether the argument text opens the one JSON object this form spells.
pub fn is_json(tail: &str) -> bool {
    tail.starts_with('{')
}

/// Parses the argument text as the one object it must be.
///
/// # Invariant
///
/// Strict here, unlike the completion's split: a submission binds real values, so
/// anything but a whole JSON object is refused with the parser's own reason.
pub fn parse_object(tail: &str) -> Result<Map<String, Value>, String> {
    let value: Value = serde_json::from_str(tail)
        .map_err(|e| format!("the arguments must be one JSON object: {e}"))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err("the arguments must be a JSON object: {...}".to_string()),
    }
}

/// One JSON value as the wire carries it: a string is itself, anything else its
/// compact JSON text.
///
/// # Invariant
///
/// The same reading the server's `parse_cmd` gives a call's arguments, so a value
/// completed here is the value the server binds there.
pub fn word(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The object's fields as wire arguments.
pub fn args_of(map: &Map<String, Value>) -> Args {
    map.iter()
        .map(|(name, value)| (name.clone(), word(value)))
        .collect()
}

/// Renders a command back into the one-object form, for the echoed line.
pub fn render(name: &str, map: &Map<String, Value>) -> String {
    format!("/{name}{}", Value::Object(map.clone()))
}
