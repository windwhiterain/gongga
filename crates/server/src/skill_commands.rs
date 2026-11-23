//! One catalog row per function a package skill publishes.
//!
//! See `docs/notes/server-content-commands.md`.

use protocol::{ArgSpec, CommandSpec, CommandTarget};
use serde_json::Value;

/// Every function every package skill publishes, under the function's own name.
///
/// # Invariant
///
/// The row carries the skill it belongs to (`CommandSpec.skill`): a front-end fills
/// `Cmd.skill` from it, so the wire states which skill answered. A name this build
/// already answers is not a skill's to take, and a second skill publishing it is left
/// out. The arguments are the generated schema's own, never a second opinion about
/// the signature: see `skill::schema_of`.
pub fn rows(known: &[CommandSpec]) -> Vec<CommandSpec> {
    let mut out: Vec<CommandSpec> = Vec::new();
    for skill in crate::tools::skill::roots(None).all() {
        if !::skill::is_package(skill.dir()) {
            continue;
        }
        let document = match ::skill::schema_of(skill.dir()) {
            Ok(document) => document,
            Err(why) => {
                tracing::warn!(
                    skill = %skill.name,
                    "this skill publishes no schema, so it offers no command rows: {why}"
                );
                continue;
            }
        };
        let Some(functions) = document.get("functions").and_then(Value::as_object) else {
            continue;
        };
        for (function, schema) in functions {
            let taken = known.iter().any(|spec| spec.name == *function)
                || out.iter().any(|spec| spec.name == *function);
            if taken {
                continue;
            }
            out.push(CommandSpec {
                skill: Some(skill.name.clone()),
                target: CommandTarget::Server,
                name: function.clone(),
                description: format!("Run `{function}` of the `{}` skill.", skill.name),
                args: arguments(schema),
            });
        }
    }
    out
}

/// One function's arguments, as the schema beside its package states them.
fn arguments(schema: &Value) -> Vec<ArgSpec> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| {
            let (completion, hint) = crate::tools::spec::worded(name);
            ArgSpec {
                type_of: type_of(property),
                name: name.clone(),
                description: property
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                required: required.contains(&name.as_str()),
                positional: false,
                value_hint: hint.map(str::to_string),
                completions: completion.source(),
                kind: None,
                accepts: None,
                sticky_to: None,
                schema: structured(property),
                update: false,
            }
        })
        .collect()
}

/// The SPEC type a schema property is: the one table reads both spellings.
fn type_of(property: &Value) -> String {
    let stated = property
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("str");
    if stated == "array" {
        if let Some(item) = property
            .get("items")
            .and_then(|items| items.get("type"))
            .and_then(Value::as_str)
        {
            return format!("list[{item}]");
        }
    }
    stated.to_string()
}

/// A structured value's own shape, so completion inside it reads the same document.
fn structured(property: &Value) -> Option<Value> {
    match property.get("type").and_then(Value::as_str) {
        Some("object") | Some("array") => Some(property.clone()),
        _ => None,
    }
}
