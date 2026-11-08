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

/// The JSON type an argument is written as when the row does not say.
fn string_type() -> String {
    "string".to_string()
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
    /// The JSON type: `string`, `integer`, `number`, `boolean` or `array`.
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

/// What carries a call that says `background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Carrier {
    /// A process of its own, which outlives a `cancel`.
    Process,
    /// A worker process that leaves its answer in a file.
    Worker,
    /// Neither: it acts on what this server holds, so `background` is refused.
    #[default]
    Here,
}

impl Carrier {
    pub fn carried(self) -> crate::tools::tunnel::Carried {
        match self {
            Carrier::Process => crate::tools::tunnel::Carried::Process,
            Carrier::Worker => crate::tools::tunnel::Carried::Worker,
            Carrier::Here => crate::tools::tunnel::Carried::Here,
        }
    }
}

/// What a name is, as `[channel.<name>]` declares it.
///
/// # Invariant
///
/// The one place a name is known: whether it is a program or a cmd, how it starts and
/// how a reply is framed. A name absent here is raw.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSpec {
    #[serde(default)]
    pub kind: Kind,
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

/// What a name is: a program this server starts, or a cmd it answers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A program on the session's machine, started and relayed.
    #[default]
    Program,
    /// A cmd of this server, answered in this process: no program, no pipes.
    Cmd,
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
    /// Whether `background` is this tool's own work.
    #[serde(default)]
    pub handles_background: bool,
    /// Whether this tool's wait is its own, so the tunnel's clock does not end it.
    #[serde(default)]
    pub owns_its_wait: bool,
    /// Whether the model is offered this tool; the command layer reaches all of them.
    #[serde(default = "yes")]
    pub offered: bool,
    /// Whether the tunnel's shared arguments (`description`, `max_output`,
    /// `timeout_ms`, `background`) belong to it.
    #[serde(default = "yes")]
    pub tunneled: bool,
    /// What carries a call of its own that says `background`.
    #[serde(default)]
    pub carrier: Carrier,
    /// The wait a call that states none gets.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Output longer than this is capped for it. `0` disables the cap.
    #[serde(default)]
    pub max_output_chars: Option<usize>,
}

impl ToolSpec {
    /// The model-facing schema: what the row declares, and nothing else.
    pub fn schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();
        for argument in &self.parameters {
            // `a|b` is a union: JSON Schema spells it as a list of types.
            let mut property = match argument.kind.split('|').collect::<Vec<_>>().as_slice() {
                [one] => json!({ "type": one }),
                several => json!({ "type": several }),
            };
            if let Some(items) = &argument.items {
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
