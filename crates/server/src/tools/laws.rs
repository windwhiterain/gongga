//! The arguments every call carries, whatever the tool: one list, one reader.

use serde_json::{Value, json};

/// Every name a call may carry besides the arguments its own row lists.
///
/// # Invariant
///
/// One list, and every door reads it: the schema, the operator's line, the check, the
/// reader. A sentence belongs here rather than on each tool's row.
pub const ALL: &[(&str, &str, &str)] = &[
    (
        "description",
        "string",
        "What this call is for, in a few words. Optional, and never executed: it also names \
         the file a capped answer is spilled to.",
    ),
    (
        "max_output",
        "integer",
        "The longest answer this call returns, in characters. Past it the middle is cut and \
         the whole of it goes to a file the answer names.",
    ),
];

/// A schema with every shared name declared, and no `null` in any kind.
///
/// # Invariant
///
/// Injected in the model-facing schema only, so a provider's validator accepts what the
/// call layer reads. A property the row already declares wins. `null` comes out of every
/// kind: a provider compiles a union only when the other branch is a string. Two kinds are
/// offered as none -- constrained decoding compiles no union; the sentence says both.
pub fn schema(declared: Value) -> Value {
    let mut schema = declared;
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, kind, _) in ALL {
            properties
                .entry((*name).to_string())
                .or_insert_with(|| json!({ "type": kind }));
        }
        for property in properties.values_mut() {
            without_null(property);
            without_union(property);
        }
    }
    schema
}

/// One property without a union: two kinds are stated as no kind, which takes any value.
fn without_union(property: &mut Value) {
    let Some(Value::Array(kinds)) = property.get("type") else {
        return;
    };
    if kinds.len() > 1 {
        if let Some(table) = property.as_object_mut() {
            table.remove("type");
        }
    }
}

/// One property without `null`: no model states it, and no provider compiles it beside a number.
fn without_null(property: &mut Value) {
    let Some(Value::Array(kinds)) = property.get("type") else {
        return;
    };
    let mut kinds = kinds.clone();
    kinds.retain(|kind| kind.as_str() != Some("null"));
    if kinds.len() == 1 {
        property["type"] = kinds.remove(0);
    } else if !kinds.is_empty() {
        property["type"] = Value::Array(kinds);
    }
}

/// What one call said about the shared arguments: its title, and its cap.
#[derive(Debug, Default, Clone)]
pub struct Given {
    /// The call's title, in the caller's own words.
    pub description: Option<String>,
    /// The longest answer this call returns, as a number of characters.
    pub max_output: Option<usize>,
}

impl Given {
    /// The one cap: the call's number, or the home's default, in characters.
    ///
    /// # Invariant
    ///
    /// Read once, by the layer that takes the shared arguments: a tool that capped too
    /// would be a second answer, and the smaller would win. `0` from either is no cap.
    pub fn cap(&self, home_default: usize) -> usize {
        self.max_output.unwrap_or(home_default)
    }

    /// Reads them off a call's arguments, and takes them off.
    ///
    /// # Invariant
    ///
    /// Before anything else reads a call's arguments: a command would answer for a name
    /// its handler has no parameter for, and a skill's function cannot bind one at all.
    /// Taking them is what makes the list the whole story -- nothing downstream has to
    /// know a shared name.
    pub fn take(args: &mut Value) -> Self {
        let mut given = Self::default();
        let Value::Object(table) = args else {
            return given;
        };
        for (name, _, _) in ALL {
            let Some(value) = table.remove(*name) else {
                continue;
            };
            match *name {
                // A count written as one: a provider may hand a number back as text.
                "max_output" => {
                    given.max_output = value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
                        .map(|max| max as usize)
                }
                _ => {
                    given.description = value
                        .as_str()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string)
                }
            }
        }
        given
    }
}
