//! The tool commands: every tool the model can call is also a command an
//! operator can type, because a tool is a tunnel into a command, not a second
//! surface.
//!
//! See `docs/notes/server-tool-cmd.md`.
//!
//! # Invariant
//!
//! The command is the source of truth and the tool is one way to reach it: the
//! agent's structured arguments pass through without escaping, the session
//! being the caller's own. One runner builds the context a command names, so
//! the cmd dispatch, a subscription's action, and the model's own call cannot
//! drift into three behaviors.

use protocol::Args;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::commands::{Dispatch, Outcome};
use crate::session::SessionManager;
use crate::store::Store;

/// Runs one tool for one session, wherever a command needs it: the tool cmds
/// themselves, and a subscription's `--bash`/`--python` action.
///
/// # Invariant
///
/// No turn exists on this path, so nothing here streams: `events` is `None` and
/// the whole outcome is the answer. The session's own shells and interpreters
/// are used, keyed as the tools key them ({session, name}), so a command and
/// the model meet in the same persistent shell.
pub async fn run(
    agent: &crate::agent::Agent,
    sessions: &SessionManager,
    store: &Store,
    session: &str,
    tool: &str,
    args: Value,
) -> Outcome {
    let handle = match sessions.get(session) {
        Ok(handle) => handle,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let model = handle.tuning().model.unwrap_or_default();
    let ctx = crate::tools::ToolContext {
        session: session.to_string(),
        call_id: format!("cmd-{}", uuid::Uuid::new_v4().simple()),
        output_dir: agent.output_dir.clone(),
        shells: handle.shells(),
        pythons: handle.pythons(),
        reader: handle.reader(),
        machine: handle.owner(),
        remote: handle.remote_name(),
        detached: agent.detached.clone(),
        events: None,
        cancel: CancellationToken::new(),
        store: store.clone(),
        cwd: handle.cwd(),
        vision: crate::llm::Vision {
            accepts_image: false,
            refusal: String::new(),
        },
        inbox: handle.inbox_signal(),
        rates: agent.rates(&model),
        model,
        closing: handle.closing_signal(),
    };
    let outcome = agent.tools.run(tool, args, &ctx).await;
    match outcome.ok {
        true => Outcome::ok(outcome.output),
        false => Outcome::err(outcome.output),
    }
}

/// The commands the tools publish: one entry per tool, `<session>` first, then
/// the tool's own arguments in its schema's order. Positional spelling is the
/// table's, because the schema says what a value means, not how a line reads.
///
/// # Invariant
///
/// Generated from the registry, not written twice: the model's view and the
/// operator's view of one tool are one definition. A tool added to the registry
/// is a command the next greeting names.
pub fn catalog(tools: &crate::tools::ToolRegistry) -> Vec<protocol::CommandSpec> {
    tools
        .iter()
        .map(|tool| {
            let schema = tool.parameters();
            let positional = positional_of(tool.name());
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut args = vec![protocol::ArgSpec {
                name: "session".into(),
                description: "The session the tool runs as; its shells, its directory.".into(),
                required: true,
                positional: true,
                value_hint: Some("name".into()),
                completions: protocol::CompletionSource::Sessions,
                sticky: false,
            }];
            for (name, spec) in &properties {
                let is_positional = positional.contains(&name.as_str());
                args.push(protocol::ArgSpec {
                    name: name.clone(),
                    description: spec
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    required: required.contains(&name.as_str()),
                    positional: is_positional,
                    value_hint: Some(
                        spec.get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("value")
                            .to_string(),
                    ),
                    completions: protocol::CompletionSource::None,
                    sticky: false,
                });
            }
            protocol::CommandSpec {
                name: tool.name().to_string(),
                description: format!("Run the `{}` tool for one session.", tool.name()),
                args,
                target: protocol::CommandTarget::Server,
            }
        })
        .collect()
}

/// Which of a tool's arguments its command spells positionally, in order: the
/// values a caller thinks of as the sentence's object, not its switches.
fn positional_of(tool: &str) -> &'static [&'static str] {
    match tool {
        "bash" | "remote_bash" => &["command"],
        "python" | "remote_python" => &["code"],
        "del_bash" | "del_python" | "list_bash" | "list_python" => &["name_regex"],
        "read_file" => &["path"],
        "write_file" => &["path", "content"],
        "replace" => &["path", "old", "new"],
        _ => &[],
    }
}

/// Turns a command's string arguments into the JSON a tool reads: booleans and
/// numbers are typed by the tool's own schema, everything else stays text.
///
/// # Invariant
///
/// The tunnel's whole point: an agent's structured call and an operator's line
/// reach the same tool with the same shapes, so a value that means a number is
/// a number on both paths.
fn coerce(tool: &dyn crate::tools::Tool, args: &Args) -> Value {
    let properties = tool
        .parameters()
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = json!({});
    for (name, value) in args.iter() {
        let kind = properties
            .get(name)
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str);
        out[name] = match kind {
            Some("boolean") => match value.parse::<bool>() {
                Ok(parsed) => json!(parsed),
                Err(_) => json!(value),
            },
            Some("integer") => value
                .parse::<i64>()
                .map(|n| json!(n))
                .unwrap_or_else(|_| json!(value)),
            Some("number") => match value.parse::<f64>() {
                Ok(parsed) => json!(parsed),
                Err(_) => json!(value),
            },
            _ => json!(value),
        };
    }
    out
}

/// Runs one tool command: binds the session, coerces what remains, and answers
/// with what the tool said.
pub async fn dispatch_tool(d: &Dispatch, name: &str, args: &Args) -> Outcome {
    let Some(session) = args.get("session") else {
        return Outcome::err(format!("{name} needs the session it runs as"));
    };
    let Some(tool) = d.agent.tools.get(name) else {
        return Outcome::err(format!("{name} is not a tool this server carries"));
    };
    let rest: Args = args
        .iter()
        .filter(|(key, _)| key.as_str() != "session")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let value = coerce(tool, &rest);
    run(&d.agent, &d.sessions, &d.store, session, name, value).await
}
