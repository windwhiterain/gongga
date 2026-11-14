//! SPEC's argument types: the one table that says what a type is, in JSON.
//!
//! See `docs/notes/protocol-message.md`.

use serde_json::Value;

/// What a call said about one argument, read from the JSON object a call is.
pub fn given<'a>(args: &'a Value, key: &str) -> crate::Given<'a> {
    match args.get(key) {
        None => crate::Given::Nothing,
        Some(Value::Null) => crate::Given::Null,
        Some(value) => crate::Given::Value(value),
    }
}

/// The JSON Schema `type` a SPEC argument type is: `optional[path]` is a string that
/// may be `null`.
///
/// # Invariant
///
/// One table, three readers: the schema a model is offered, the check a call is held
/// to, and the value an operator's typed line becomes. A spelling this table does not
/// know is an error, never a silent `string`.
pub fn schema_types(spec: &str) -> Result<Vec<&'static str>, String> {
    let mut found: Vec<&'static str> = Vec::new();
    for part in union(spec) {
        let (part, optional) = optional_of(&part);
        for one in base(&part)? {
            push(&mut found, one);
        }
        if optional {
            push(&mut found, "null");
        }
    }
    Ok(found)
}

/// Whether a stated `null` is a value this type admits.
pub fn admits_null(spec: &str) -> Result<bool, String> {
    Ok(schema_types(spec)?.contains(&"null"))
}

/// What a `list[...]` holds, when it says: `list[str]` holds strings.
pub fn item_type(spec: &str) -> Option<&'static str> {
    let text = spec.trim();
    let inner = text
        .strip_prefix("list[")
        .and_then(|r| r.strip_suffix(']'))?;
    base(inner).ok().and_then(|types| types.first().copied())
}

/// One word of an operator's line as the value its own argument declares.
///
/// # Invariant
///
/// A line spells every argument as text, and the shape is the argument's, not the
/// line's: a word an integer argument cannot hold is an error here rather than a string
/// that reaches the command as the wrong type.
pub fn value_of(spec: &str, word: &str) -> Result<Value, String> {
    let types = schema_types(spec)?;
    let text = word.trim();
    if types.contains(&"object") && text.starts_with('{')
        || types.contains(&"array") && text.starts_with('[')
    {
        return serde_json::from_str::<Value>(text)
            .map_err(|e| format!("{word:?} is not the JSON its argument takes: {e}"));
    }
    if types.contains(&"boolean")
        && let Ok(flag) = text.parse::<bool>()
    {
        return Ok(Value::Bool(flag));
    }
    if (types.contains(&"integer") || types.contains(&"number"))
        && let Ok(number) = text.parse::<serde_json::Number>()
    {
        return Ok(Value::Number(number));
    }
    if !types.contains(&"string") {
        return Err(format!("{word:?} is not one of the types {spec} takes"));
    }
    Ok(Value::String(word.to_string()))
}

/// The parts of a `|` union, ignoring the ones nested inside brackets.
fn union(spec: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0;
    let mut part = String::new();
    for c in spec.trim().chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '|' if depth == 0 => {
                parts.push(part.trim().to_string());
                part.clear();
                continue;
            }
            _ => {}
        }
        part.push(c);
    }
    parts.push(part.trim().to_string());
    parts.retain(|part| !part.is_empty());
    parts
}

/// `optional[X]` as `X` and a flag, and anything else as itself.
fn optional_of(spec: &str) -> (String, bool) {
    let text = spec.trim();
    for prefix in ["optional[", "option["] {
        if let Some(inner) = text.strip_prefix(prefix).and_then(|r| r.strip_suffix(']')) {
            return (inner.trim().to_string(), true);
        }
    }
    (text.to_string(), false)
}

/// The JSON types one SPEC type name is.
///
/// # Invariant
///
/// SPEC's aliases say what a value *is*, not how it travels: a `path`, a `Channel`, a
/// `Session`, a `Skill`, a `Pattern` and a `Range` are all strings, and a `Cmd`, an
/// `Event` and a `SessionConfig` are objects.
fn base(spec: &str) -> Result<Vec<&'static str>, String> {
    let name = spec.trim();
    let types: Vec<&'static str> = match name {
        "str" | "string" | "path" | "name" => vec!["string"],
        "Channel" | "Session" | "Skill" | "Subscription" | "Pattern" | "Range" => vec!["string"],
        // SPEC's own names for the values a `SessionConfig` holds.
        "Thinking" | "Remote" => vec!["string"],
        "int" | "integer" | "time" => vec!["integer"],
        "float" | "number" => vec!["number"],
        "bool" | "boolean" => vec!["boolean"],
        "dict" | "dict[str, any]" | "object" | "Cmd" | "Event" | "SessionConfig" => vec!["object"],
        "Model" | "ModelConfig" | "Workspace" | "LiveCompact" => vec!["object"],
        other if other.starts_with("list[") || other == "array" => vec!["array"],
        other if other.starts_with("dict[") => vec!["object"],
        other => {
            return Err(format!(
                "{other:?} is not a type SPEC names: write one of str, int, number, bool, \
                 path, list[...], dict[...], optional[...], or a `|` of them"
            ));
        }
    };
    Ok(types)
}

fn push(found: &mut Vec<&'static str>, one: &'static str) {
    if !found.contains(&one) {
        found.push(one);
    }
}
