//! The input line's grammar and sticky entries, without a terminal.
//!
//! See `docs/notes/client-window-input.md`.

use crate::window_command::{self as command, Done};
use crate::window_state::{Follow, State};
use crate::{Client, LineKind, Sink, Submission};

/// What an input line asks for: `/` starts a command, anything else is prose for
/// the agent.
///
/// # Invariant
///
/// A line this front-end generates for itself -- a click -- is written as a
/// command, because prose here is a message the operator never typed.
fn classified(line: &str) -> Option<Result<Vec<String>, String>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .strip_prefix('/')
        .map(|command| shell_words::split(command).map_err(|e| format!("unparsable input: {e}")))
}

/// One input line, answered.
///
/// # Invariant
///
/// A line the window runs itself never reaches the command layer, and a line the
/// server runs carries every argument the operator meant: the sticky entries are
/// filled here, where they are kept, and a filled value nothing accepts is left
/// out rather than sent as a statement the operator never made.
pub fn submission(
    client: &mut Client,
    state: &mut State,
    line: &str,
    sink: &mut dyn Sink,
    moved: &mut Option<std::net::SocketAddr>,
) -> Submission {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Submission::Rejected;
    }
    match classified(line) {
        Some(Ok(tokens)) => {
            let head = tokens.first().cloned().unwrap_or_default();
            if let Some(spec) = command::spec(&head) {
                client.echo(&tokens, None, sink);
                return match crate::commands::parse_args(&tokens[1..], Some(&spec)) {
                    Ok(args) => {
                        if let Done::Moved(addr) = command::run(client, state, &head, &args, sink) {
                            *moved = Some(addr);
                        }
                        Submission::Local
                    }
                    Err(e) => {
                        client.say(LineKind::Error, &e.to_string(), sink);
                        Submission::Rejected
                    }
                };
            }
            client.command(filled(client, state, tokens, sink), sink)
        }
        Some(Err(e)) => {
            sink.note(LineKind::Error, &e);
            Submission::Rejected
        }
        None => match state.sticky().get("session") {
            Some(session) => client.message(trimmed, session, sink),
            None => {
                client.say(
                    LineKind::Error,
                    "no session to send to: set one with `/set_sticky session <name>`, or start the line with `/` and name the command",
                    sink,
                );
                Submission::Rejected
            }
        },
    }
}

/// One command line with the sticky entries filled in.
///
/// # Invariant
///
/// A value lands on a slot that is still empty, and only through the argument
/// whose `kind` names it; what the caller typed wins. Nothing filled may change
/// what an existing session holds -- a fill that disagrees with the row is left
/// out *and said*, like one the published `accepts` refuses. A session the line
/// creates takes the entries as they stand. See `docs/notes/tui-run.md`.
fn filled(client: &Client, state: &State, tokens: Vec<String>, sink: &mut dyn Sink) -> Vec<String> {
    let head = tokens.first().cloned().unwrap_or_default();
    let Some(spec) = client.commands().into_iter().find(|spec| spec.name == head) else {
        return tokens;
    };
    let Ok(args) = crate::commands::parse_args(&tokens[1..], Some(&spec)) else {
        // The command layer's own diagnosis is better than none: let it speak.
        return tokens;
    };
    let mut merged = args.clone();
    state.sticky().fill(Some(&spec), &mut merged);
    // Which session this line acts on, entries and all: the answer `follow` keeps.
    let acting = client.session_named(&head, &merged).map(str::to_string);
    // Its row, when it already exists: the one thing a fill has to agree with.
    let existing = acting
        .as_deref()
        .and_then(|name| client.sessions().iter().find(|info| info.name == name));
    let line = format!("/{}", tokens.join(" "));
    let mut filled = tokens;
    let mut left = Vec::new();
    for arg in &spec.args {
        // Typed, or no entry names it: nothing to add.
        if args.contains_key(&arg.name) {
            continue;
        }
        let Some(value) = merged.get(&arg.name).cloned() else {
            continue;
        };
        // An entry that would change what the session already holds.
        if !arg.required
            && let Some(info) = existing
            && !restates(info, arg.kind.as_deref(), &value)
        {
            left.push(arg.kind.clone().unwrap_or_else(|| arg.name.clone()));
            continue;
        }
        if client
            .refused_values(&line, &merged)
            .iter()
            .any(|(name, _)| *name == arg.name)
        {
            sink.note(
                LineKind::System,
                &format!(
                    "sticky {} = {value} is not one this model takes; left to the model's own",
                    arg.kind.as_deref().unwrap_or(&arg.name)
                ),
            );
            continue;
        }
        // A flag, whatever its position: filled, not re-ordered, and one token.
        filled.push(format!("--{}", arg.name));
        filled.push(value);
    }
    if let (Some(acting), false) = (acting, left.is_empty()) {
        sink.note(
            LineKind::System,
            &format!(
                "sticky {} left off: {acting} does not hold {}",
                left.join(", "),
                if left.len() == 1 { "it" } else { "them" }
            ),
        );
    }
    filled
}

/// Whether an entry restates what a session's row already says.
///
/// # Invariant
///
/// Only a restatement is filled into a line about a session that exists: an entry
/// that disagrees would rewrite a setting nobody stated on this line. A kind this
/// function does not know answers `false` -- filling it would change something it
/// cannot check.
fn restates(info: &crate::protocol::SessionInfo, kind: Option<&str>, value: &str) -> bool {
    match kind {
        Some("model") => info.model.as_deref() == Some(value),
        Some("thinking") => info.thinking_effort.as_deref() == Some(value),
        // The row holds the percentage, the entry spells it `50%`.
        Some("ctx_limit") => {
            let percent = value.trim_end_matches('%').parse::<u8>().ok();
            info.context_limit == percent
        }
        _ => false,
    }
}

/// What the window keeps for a command it is about to send.
///
/// # Invariant
///
/// Two things, both SPEC's: a session the command names is drawn and becomes the
/// sticky one, and an accepted `set_session` writes back every mode it carried.
/// Neither is written before the answer -- a refused command writes nothing.
pub fn follow(
    client: &Client,
    state: &mut State,
    msg: &crate::protocol::ClientMessage,
    line: &str,
    pending: &mut Vec<(u64, Follow)>,
) {
    let crate::protocol::ClientMessage::Command { id, name, args } = msg else {
        return;
    };
    if let Some(session) = client.session_named(name, args).map(str::to_string) {
        if !state.is_shown(&session) {
            let _ = state.set_shown(&session, true);
        }
        let _ = state.set_sticky("session", &session);
    }
    if name == "set_session"
        && let Some(spec) = client
            .commands()
            .into_iter()
            .find(|spec| spec.name == *name)
    {
        // Only what was typed: a filled value is already in the file.
        let typed = typed_names(client, line);
        let pairs: Vec<(String, String)> = spec
            .args
            .iter()
            .filter(|arg| typed.contains(&arg.name))
            .filter_map(|arg| {
                let kind = arg.kind.clone()?;
                let value = args.get(&arg.name)?.clone();
                Some((kind, value))
            })
            .collect();
        if !pairs.is_empty() {
            pending.push((*id, pairs));
        }
    }
}

/// The argument names a line spelled out, before any sticky filled one in.
fn typed_names(client: &Client, line: &str) -> Vec<String> {
    let Some(Ok(tokens)) = classified(line) else {
        return Vec::new();
    };
    let head = tokens.first().cloned().unwrap_or_default();
    let Some(spec) = client.commands().into_iter().find(|spec| spec.name == head) else {
        return Vec::new();
    };
    crate::commands::parse_args(&tokens[1..], Some(&spec))
        .map(|args| args.keys().cloned().collect())
        .unwrap_or_default()
}
