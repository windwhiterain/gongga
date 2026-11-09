//! The tool commands: every tool is also a command an operator can type.
//!
//! See `docs/notes/server-tool-cmd.md`.

use protocol::Args;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::commands::{Dispatch, Outcome};
use crate::session::SessionManager;
use crate::store::Store;

/// Runs one tool for one session: the tool cmds, and a subscription's action.
///
/// # Invariant
///
/// Nothing here streams: `events` is `None` and the whole outcome is the
/// answer. The session's own shells are used, keyed as the tools key them, so
/// a command and the model meet in the same persistent shell.
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
    let ctx = crate::tools::ToolContext {
        session: session.to_string(),
        call_id: format!("cmd-{}", uuid::Uuid::new_v4().simple()),
        tool: tool.to_string(),
        channels: agent.channels.clone(),
        home: agent.home.clone(),
        output_dir: agent.output_dir.clone(),
        workspace: handle.workspace(),
        reader: handle.reader(),
        machine: handle.owner(),
        remote: handle.remote_name(),
        jobs: agent.jobs.clone(),
        events: None,
        cancel: CancellationToken::new(),
        store: store.clone(),
        cwd: handle.cwd(),
        env: handle.env(),
        vision: crate::llm::Vision {
            accepts_image: false,
            refusal: String::new(),
        },
        inbox: handle.inbox_signal(),
        // An operator's line holds this turn the same way a model's call does.
        holds_the_turn: true,
        stop: handle.stop_signal(),
        closing: handle.closing_signal(),
        explore: handle.explore(),
    };
    let outcome = agent.tools.run(tool, args, &ctx).await;
    match outcome.ok {
        true => Outcome::ok(outcome.output),
        false => Outcome::err(outcome.output),
    }
}

/// The commands the tools publish: one entry per tool, `<session>` first.
///
/// # Invariant
///
/// Generated from the registry: the model's view and the operator's view of
/// one tool are one definition. Positional spelling is the table's, not the
/// schema's.
pub fn catalog(tools: &crate::tools::ToolRegistry) -> Vec<protocol::CommandSpec> {
    tools
        .iter()
        .map(|tool| {
            let row = tool.spec();
            let mut args = vec![protocol::ArgSpec {
                name: "session".into(),
                description: "The session the tool runs as; its shells, its directory.".into(),
                required: true,
                positional: true,
                kind: None,
                value_hint: Some("name".into()),
                completions: protocol::CompletionSource::Sessions,
                accepts: None,
                sticky_to: None,
                sticky: false,
            }];
            // The row's order, object first: a bare word binds the first unfilled one.
            let mut ordered: Vec<&crate::tools::spec::Argument> = row
                .parameters
                .iter()
                .filter(|argument| argument.positional.is_some())
                .collect();
            ordered.sort_by_key(|argument| argument.positional);
            ordered.extend(
                row.parameters
                    .iter()
                    .filter(|argument| argument.positional.is_none()),
            );
            for argument in ordered {
                args.push(protocol::ArgSpec {
                    name: argument.name.clone(),
                    description: argument.sentence.clone().unwrap_or_default(),
                    required: argument.required,
                    positional: argument.positional.is_some(),
                    kind: Some(argument.kind.clone()),
                    value_hint: argument.hint.clone(),
                    completions: argument.completion.source(),
                    accepts: None,
                    sticky_to: None,
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

/// Types a command's string arguments by the tool's own schema.
///
/// # Invariant
///
/// The tunnel's point: an agent's call and an operator's line reach the same
/// shapes, so a number is a number on both paths.
fn coerce(tool: &dyn crate::tools::Tool, args: &Args) -> Value {
    let properties = crate::tools::general_parameters(tool)
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = json!({});
    for (name, value) in args.iter() {
        let spec = properties.get(name);
        let kind = spec.and_then(|p| p.get("type")).and_then(Value::as_str);
        // A list argument is written as JSON, so an operator reaches the same
        // shape the model does.
        if kind == Some("array")
            && let Some(words) = serde_json::from_str::<Vec<String>>(value).ok()
        {
            out[name] = json!(words);
            continue;
        }
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
    let value = coerce(tool.as_ref(), &rest);
    run(&d.agent, &d.sessions, &d.store, session, name, value).await
}
