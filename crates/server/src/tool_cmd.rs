//! The tool commands: every tool is also a command an operator can type.
//!
//! See `docs/notes/server-tool-cmd.md`.

use protocol::Args;
use serde_json::Value;
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
    let ctx = match context(agent, sessions, store, session, tool) {
        Ok(ctx) => ctx,
        Err(why) => return Outcome::err(why),
    };
    let outcome = agent.tools().run(tool, args, &ctx).await;
    match outcome.ok {
        true => Outcome::ok(outcome.output),
        false => Outcome::err(outcome.output),
    }
}

/// The context one call runs as: the session's shells, its directory, its machine.
///
/// # Invariant
///
/// One construction, shared with the command that runs a skill (`skill_command`): a
/// tool's call and the command layer's reach the same session state, keyed the same way.
pub fn context(
    agent: &crate::agent::Agent,
    sessions: &SessionManager,
    store: &Store,
    session: &str,
    tool: &str,
) -> Result<crate::tools::ToolContext, String> {
    let handle = sessions.get(session).map_err(|e| e.to_string())?;
    Ok(crate::tools::ToolContext {
        session: session.to_string(),
        call_id: format!("cmd-{}", uuid::Uuid::new_v4().simple()),
        tool: tool.to_string(),
        channels: agent.channels.clone(),
        home: agent.home.clone(),
        output_dir: agent.output_dir.clone(),
        workspace: handle.workspace(),
        space: handle.space(),
        machine: handle.owner(),
        remote: handle.remote_name(),
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
        stop: handle.stop_signal(),
        closing: handle.closing_signal(),
        disk_low: agent.config.now().disk_low,
        explore: handle.explore(),
    })
}

/// The commands the tools publish: one entry per tool, `<session>` first.
///
/// # Invariant
///
/// Generated from the registry: the model's view and the operator's view of one tool are
/// one definition. `session` is never required: it is the caller's own unless the call
/// names another (SPEC's `session = caller()`). A tool that only forwards its call is not
/// here -- see `server-tool-cmd.md`: its entry would resolve back to itself.
pub fn catalog(tools: &crate::tools::ToolRegistry) -> Vec<protocol::CommandSpec> {
    tools
        .iter()
        .filter(|tool| tool.forwards_to().is_none())
        .map(|tool| {
            let row = tool.spec();
            let mut args = vec![protocol::ArgSpec {
                type_of: "Session".into(),
                name: "session".into(),
                description: "The session the tool runs as; its shells, its directory. \
                     Omitted is the caller's own session."
                    .into(),
                required: false,
                positional: true,
                kind: Some("session".into()),
                value_hint: Some("name".into()),
                completions: protocol::CompletionSource::Sessions,
                accepts: None,
                sticky_to: None,
                schema: None,
                update: false,
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
                    type_of: argument.kind.clone(),
                    name: argument.name.clone(),
                    description: argument.sentence.clone().unwrap_or_default(),
                    required: argument.required,
                    positional: argument.positional.is_some(),
                    kind: None,
                    value_hint: argument.hint.clone(),
                    completions: argument.completion.source(),
                    accepts: None,
                    sticky_to: None,
                    schema: None,
                    update: false,
                });
            }
            // The shared arguments, from the one list: an operator's line and the model's
            // schema cannot disagree.
            for (name, kind, sentence) in crate::tools::laws::ALL {
                args.push(protocol::ArgSpec {
                    type_of: (*kind).into(),
                    name: (*name).into(),
                    description: (*sentence).into(),
                    required: false,
                    positional: false,
                    kind: None,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                    accepts: None,
                    sticky_to: None,
                    schema: None,
                    update: false,
                });
            }
            protocol::CommandSpec {
                skill: None,
                name: tool.name().to_string(),
                description: format!("Run the `{}` tool for one session.", tool.name()),
                args,
                target: protocol::CommandTarget::Server,
            }
        })
        .collect()
}

/// Runs one tool command: binds the caller, and answers with what the tool said.
///
/// # Invariant
///
/// SPEC's `session = caller()`: the session a tool runs for is the caller's, never an
/// argument. The arguments that remain are the ones the tool's own schema declares.
pub async fn dispatch_tool(d: &Dispatch, name: &str, args: &Args, caller: Option<&str>) -> Outcome {
    let Some(session) = crate::commands::session_of(args, caller) else {
        return Outcome::err(format!(
            "{name} needs the session it runs as: run it from a session's shell, where `GONGGA_SESSION` names one"
        ));
    };
    if d.agent.tools().get(name).is_none() {
        return Outcome::err(format!("{name} is not a tool this server carries"));
    }
    let mut given = args.clone();
    given.remove("session");
    run(
        &d.agent,
        &d.sessions,
        &d.store,
        &session,
        name,
        given.as_json(),
    )
    .await
}
