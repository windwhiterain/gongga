//! One tool as a home's config declares it: its name, its arguments, and the
//! command that does the work.
//!
//! See `docs/notes/server-tools-spec.md`.
//!
//! # Invariant
//!
//! Everything said *about* a tool is here and in the file it is read from: the name,
//! the shape of the arguments, the sentence the model reads for each of them, which
//! machine it drives, and whether its call writes, backgrounds, keeps its own clock
//! and takes the tunnel's shared arguments. The code holds the command that runs and
//! nothing else, so a tool is added, reworded or given an argument without a build.

use serde::Deserialize;
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Argument {
    /// The name both the model and the operator spell.
    pub name: String,
    /// The JSON type: `string`, `integer`, `number`, `boolean` or `array`.
    #[serde(rename = "type", default = "string_type")]
    pub kind: String,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
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
        }
    }
}

/// Which machine a tool's paths belong to, as the row spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
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

/// One tool, exactly as one row under `[tools]` declares it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// The command this tool hands its arguments to: the one implementation that
    /// knows how to do the work. A row naming no command this build carries is
    /// refused at startup, by name.
    pub cmd: String,
    /// Which machine its paths belong to, and which twin it is.
    #[serde(default)]
    pub machine: Side,
    /// What the tool is, in the one sentence the model reads. Written here and
    /// nowhere else: a schema with an argument and no sentence is a model guessing.
    #[serde(default)]
    pub summary: Option<String>,
    /// Its arguments, in the order an operator spells them.
    #[serde(default)]
    pub parameters: Vec<Argument>,
    /// Whether calling this tool is itself a change to the workspace, which is what
    /// a session that only looks refuses.
    #[serde(default)]
    pub writes: bool,
    /// Whether `background` is this tool's own work: a job of a shell or an
    /// interpreter, read back by that one's reading tool.
    #[serde(default)]
    pub handles_background: bool,
    /// Whether this tool's wait is its own, so the tunnel's clock does not end it.
    #[serde(default)]
    pub owns_its_wait: bool,
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
    /// The argument named, when the row declares one.
    pub fn argument(&self, name: &str) -> Option<&Argument> {
        self.parameters.iter().find(|a| a.name == name)
    }

    /// The arguments an operator spells positionally, in the row's order.
    pub fn positional(&self) -> Vec<&str> {
        let mut placed: Vec<(usize, &str)> = self
            .parameters
            .iter()
            .filter_map(|a| a.positional.map(|at| (at, a.name.as_str())))
            .collect();
        placed.sort_by_key(|(at, _)| *at);
        placed.into_iter().map(|(_, name)| name).collect()
    }

    /// The model-facing schema: what the row declares, and nothing else.
    ///
    /// The tunnel's own arguments are added by [`crate::tools::general_parameters`],
    /// which is the one place that knows every call may carry them.
    pub fn schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();
        for argument in &self.parameters {
            let mut property = json!({ "type": argument.kind });
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
