//! What a home's config declares: a tool's row, and what a channel's name is.
//!
//! See `docs/notes/server-tools-spec.md`.
//!
//! # Invariant
//!
//! Everything said about a tool is here: its name, its arguments, their sentences,
//! and every flag the call layer obeys. The code holds the command that runs.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::session::Machine;

/// The type an argument is written as when the row does not say: a word.
fn string_type() -> String {
    "str".to_string()
}

fn yes() -> bool {
    true
}

/// One argument of one tool, in the order the row lists it.
///
/// # Invariant
///
/// An order, not a table: the position an argument takes on an operator's line is
/// the row's, so the two surfaces spell one call the same way.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Argument {
    /// The name both the model and the operator spell.
    pub name: String,
    /// SPEC's type in SPEC's spelling: `time`, `list[str]`, `optional[...]`, or a `|`.
    /// `protocol::argtype` maps it to JSON.
    #[serde(rename = "type", default = "string_type")]
    pub kind: String,
    /// What an `array` argument's elements are. Only arrays have one.
    #[serde(default)]
    pub items: Option<String>,
    /// Whether a call that omits it is refused.
    #[serde(default)]
    pub required: bool,
    /// Where it sits on an operator's line, counting from one. Omitted is a switch
    /// an operator spells by name.
    #[serde(default)]
    pub positional: Option<usize>,
    /// What an operator is shown as the value's shape: `name`, `path`, and so on.
    #[serde(default)]
    pub hint: Option<String>,
    /// What a front-end completes this value against.
    #[serde(default)]
    pub completion: Completion,
    /// What the model reads about this argument.
    #[serde(default)]
    pub sentence: Option<String>,
}

/// What a value is completed against, in the row's own words.
///
/// # Invariant
///
/// The same list the protocol publishes, because a tool's argument and a command's
/// argument are completed by one client-side engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Completion {
    #[default]
    None,
    /// The live session list.
    Sessions,
    /// The models this server could send a request on.
    Models,
    /// The skills the line's session can open.
    Skills,
    /// A directory of the session the line names.
    Paths,
    /// Every argument name.
    Arguments,
    /// The machines `[remotes]` names.
    Remotes,
}

impl Completion {
    /// The source the protocol publishes for it.
    pub fn source(self) -> protocol::CompletionSource {
        match self {
            Completion::None => protocol::CompletionSource::None,
            Completion::Sessions => protocol::CompletionSource::Sessions,
            Completion::Models => protocol::CompletionSource::Models,
            Completion::Skills => protocol::CompletionSource::Skills,
            Completion::Paths => protocol::CompletionSource::Paths,
            Completion::Arguments => protocol::CompletionSource::Arguments,
            Completion::Remotes => protocol::CompletionSource::Remotes,
        }
    }
}

/// Which machine a tool's paths belong to, as the row spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Local,
    Remote,
}

impl Side {
    pub fn machine(self) -> Machine {
        match self {
            Side::Local => Machine::Local,
            Side::Remote => Machine::Remote,
        }
    }
}

/// What a name is, as `[channel.<name>]` declares it.
///
/// # Invariant
///
/// The one place a name is known: how it starts and how a reply is framed. Every name
/// is a program -- the command layer is reached by a tool call, not by a channel. A name
/// absent here is raw.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSpec {
    /// The words the program is started with, when a call gives no `args` of its own.
    #[serde(default)]
    pub spawn: Vec<String>,
    /// How an `input` is spelled: `{{input}}` stands where the input goes, and
    /// `{{sentinel}}` where this frame's marker goes.
    #[serde(default)]
    pub write: Option<String>,
    /// What ends a reply, when the call states no `until`.
    #[serde(default)]
    pub until: Option<String>,
    /// Whether the marker's own tail carries the status, which is then dropped from
    /// the window.
    #[serde(default)]
    pub exit_from_marker: bool,
}

/// One tool, exactly as one row under `[tools]` declares it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// The command this row hands its arguments to, by name.
    pub cmd: String,
    /// Which machine its paths belong to, and which twin it is.
    #[serde(default)]
    pub machine: Side,
    /// What the tool is, in the one sentence the model reads.
    #[serde(default)]
    pub summary: Option<String>,
    /// Its arguments, in the order an operator spells them.
    #[serde(default)]
    pub parameters: Vec<Argument>,
    /// Whether calling this tool is itself a change to the workspace, which is what
    /// a session that only looks refuses.
    #[serde(default)]
    pub writes: bool,
    /// Whether the model is offered this tool; the command layer reaches all of them.
    #[serde(default = "yes")]
    pub offered: bool,
}

impl ToolSpec {
    /// The row as the declared function it forwards to re-states it.
    ///
    /// # Invariant
    ///
    /// The function names its arguments, kinds them and says which are required; the row
    /// adds only what a signature cannot say -- a sentence, a hint, a place on an operator's
    /// line. A name the function does not take is dropped, and a kind the row states is
    /// ignored: one statement, or none.
    pub fn from_declared(self, declared: &[Argument]) -> Self {
        let parameters = declared
            .iter()
            .map(|given| {
                let said = self.parameters.iter().find(|row| row.name == given.name);
                if let Some(said) = said.filter(|said| !given.kind.is_empty() && said.kind != given.kind)
                {
                    tracing::debug!(
                        argument = %given.name,
                        said = %said.kind,
                        function = %given.kind,
                        "this argument is typed by the function it forwards to: the row's kind is ignored"
                    );
                }
                Argument {
                    name: given.name.clone(),
                    // Empty is the function naming no type, which is an untyped argument.
                    kind: given.kind.clone(),
                    items: given
                        .items
                        .clone()
                        .or_else(|| said.and_then(|said| said.items.clone())),
                    required: given.required,
                    positional: said.and_then(|said| said.positional),
                    hint: said.and_then(|said| said.hint.clone()),
                    completion: said.map(|said| said.completion).unwrap_or_default(),
                    sentence: said.and_then(|said| said.sentence.clone()),
                }
            })
            .collect();
        for row in &self.parameters {
            if !declared.iter().any(|given| given.name == row.name) {
                tracing::debug!(
                    argument = %row.name,
                    "this argument is not one the function takes: the row's is dropped"
                );
            }
        }
        Self { parameters, ..self }
    }

    /// The model-facing schema: what the row declares, and nothing else.
    pub fn schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();
        for argument in &self.parameters {
            let kind = argument.kind.as_str();
            let name = &argument.name;
            // SPEC's spelling, mapped once. A kind nothing named is a property with no type.
            let mut property = match protocol::argtype::schema_types(kind) {
                _ if kind.trim().is_empty() => json!({}),
                Ok(types) if types.len() == 1 => json!({ "type": types[0] }),
                Ok(types) => json!({ "type": types }),
                Err(why) => panic!("the row for {name:?} says {why}"),
            };
            if let Some(items) = protocol::argtype::item_type(kind).or(argument.items.as_deref()) {
                property["items"] = json!({ "type": items });
            }
            if let Some(sentence) = &argument.sentence {
                property["description"] = Value::String(sentence.clone());
            }
            properties.insert(argument.name.clone(), property);
            if argument.required {
                required.push(argument.name.clone());
            }
        }
        let mut schema = json!({
            "type": "object",
            "properties": Value::Object(properties),
            "additionalProperties": false,
        });
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        schema
    }
}
