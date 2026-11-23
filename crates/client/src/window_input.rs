//! The input line's grammar and sticky entries, without a terminal.

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
    // The JSON form `/name{...}`: completion offers it alone; sticky fills the
    // fields left open.
    if let Some(rest) = trimmed.strip_prefix('/') {
        let (name, tail) = crate::json_command::split(rest);
        if crate::json_command::is_json(&tail) {
            return submit_json(client, state, &name, &tail, sink, moved);
        }
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
/// whose `kind` names it; what the caller typed wins, and a structured argument
/// keeps every leaf the caller stated. A value the model refuses is left out and
/// said, because the caller never wrote it. See `docs/notes/client-window-input.md`.
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
    let line = format!("/{}", tokens.join(" "));
    let mut filled = tokens;
    for (name, value) in sticky_additions(client, &spec, &args, &merged, &line, sink) {
        // One token, where the line already spells it or at its end.
        place(&mut filled, &name, crate::json_command::word(&value));
    }
    filled
}

/// Puts one filled value where the line spells that argument, or at its end.
///
/// # Invariant
///
/// A structured argument the line stated is rewritten in place: the entries fill
/// the leaves it left open, and a second `--config` would be one argument given
/// twice -- refused by the parser the fill itself relies on.
fn place(tokens: &mut Vec<String>, name: &str, word: String) {
    let flag = format!("--{name}");
    if let Some(at) = tokens.iter().position(|token| *token == flag)
        && let Some(value) = tokens.get_mut(at + 1)
    {
        *value = word;
        return;
    }
    let spelled = format!("--{name}=");
    if let Some(at) = tokens.iter().position(|token| token.starts_with(&spelled)) {
        tokens[at] = format!("{spelled}{word}");
        return;
    }
    tokens.push(flag);
    tokens.push(word);
}

/// The sticky entries a line may take, as `(argument, value)` pairs; both
/// spellings fill by this one rule.
///
/// # Invariant
///
/// A value lands on a slot that is still empty, and only through the argument
/// whose `kind` names it; what the caller typed wins. A structured argument is
/// filled leaf by leaf, so the object the caller wrote keeps every leaf it stated.
/// A filled value the model refuses is dropped and said: the caller never wrote
/// it, so it is not theirs to answer for.
fn sticky_additions(
    client: &Client,
    spec: &protocol::CommandSpec,
    args: &protocol::Args,
    merged: &protocol::Args,
    line: &str,
    sink: &mut dyn Sink,
) -> Vec<(String, serde_json::Value)> {
    let refused = client.refused_values(line, merged);
    let mut additions = Vec::new();
    for arg in &spec.args {
        let Some(value) = merged.get(&arg.name).cloned() else {
            continue;
        };
        let stated = args.get(&arg.name);
        // A structured argument: the leaves the line left open are the entries'.
        if arg.schema.is_some() {
            if stated != Some(&value) {
                additions.push((arg.name.clone(), value));
            }
            continue;
        }
        if stated.is_some() {
            continue;
        }
        if refused.iter().any(|(name, _)| *name == arg.name) {
            sink.note(
                LineKind::System,
                &format!(
                    "sticky {} = {value} is not one this model takes; left to the model's own",
                    arg.kind.as_deref().unwrap_or(&arg.name)
                ),
            );
            continue;
        }
        additions.push((arg.name.clone(), value));
    }
    additions
}

/// Submits the JSON form `/name{...}`: the object's fields are the arguments,
/// sticky fills the ones left open.
///
/// # Invariant
///
/// The same fill rule as the flag form ([`sticky_additions`]), so a sticky entry
/// reaches a JSON line exactly as it reaches a flag line -- the mixing that dropped
/// it is gone. A client command runs here; a server command is sent bound.
fn submit_json(
    client: &mut Client,
    state: &mut State,
    name: &str,
    tail: &str,
    sink: &mut dyn Sink,
    moved: &mut Option<std::net::SocketAddr>,
) -> Submission {
    let map = match crate::json_command::parse_object(tail) {
        Ok(map) => map,
        Err(e) => {
            client.say(LineKind::Error, &e, sink);
            return Submission::Rejected;
        }
    };
    let raw = format!("/{name}{tail}");
    if command::spec(name).is_some() {
        client.echo_command(&raw, None, sink);
        let args = crate::json_command::args_of(&map);
        if let Done::Moved(addr) = command::run(client, state, name, &args, sink) {
            *moved = Some(addr);
        }
        return Submission::Local;
    }
    let args = crate::json_command::args_of(&map);
    let Some(spec) = client.commands().into_iter().find(|spec| spec.name == name) else {
        // Unknown here is the command layer's to say, with the published list.
        return client.submit_command(name.to_string(), args, raw, sink);
    };
    let mut merged = args.clone();
    state.sticky().fill(Some(&spec), &mut merged);
    let additions = sticky_additions(client, &spec, &args, &merged, &raw, sink);
    if additions.is_empty() {
        return client.submit_command(name.to_string(), args, raw, sink);
    }
    let mut filled = map.clone();
    for (field, value) in additions {
        filled.insert(field, value);
    }
    let typed = crate::json_command::render(name, &filled);
    client.submit_command(
        name.to_string(),
        crate::json_command::args_of(&filled),
        typed,
        sink,
    )
}

/// What the window keeps for a command it is about to send.
///
/// # Invariant
///
/// All SPEC's: a session the command names is drawn and becomes the sticky one;
/// an accepted `set_session` or `new_session` writes back every value the line
/// stated that an entry could fill -- a structured one leaf by leaf -- and marks
/// a session it created as this client's own. The writeback waits for the answer,
/// so a refused command writes none of it.
pub fn follow(
    client: &Client,
    state: &mut State,
    msg: &crate::protocol::ClientMessage,
    line: &str,
    pending: &mut Vec<(u64, Follow)>,
) {
    let crate::protocol::ClientMessage::Command { id, name, args, .. } = msg else {
        return;
    };
    let named = client.session_named(name, args).map(str::to_string);
    if let Some(session) = &named {
        if !state.is_shown(session) {
            let _ = state.set_shown(session, true);
        }
        let _ = state.set_sticky("session", session);
    }
    // The two commands whose arguments are a session's settings.
    if name != "set_session" && name != "new_session" {
        return;
    }
    // SPEC's client default: only a session this window creates is its own;
    // joining an existing one writes no mark.
    let created = named.filter(|session| !client.sessions().iter().any(|row| row.name == *session));
    let sticky = stated_entries(client, state, name, line);
    if !sticky.is_empty() || created.is_some() {
        pending.push((*id, Follow { sticky, created }));
    }
}

/// The sticky entries an accepted settings command writes back: only what the
/// line typed, not what a fill supplied.
fn stated_entries(client: &Client, state: &State, name: &str, line: &str) -> Vec<(String, String)> {
    let Some(spec) = client.commands().into_iter().find(|spec| spec.name == name) else {
        return Vec::new();
    };
    let Some(typed) = typed_args(client, line) else {
        return Vec::new();
    };
    let fillable = crate::commands::sticky_kinds(&client.commands());
    let mut pairs = state.sticky().absorb(&spec, &typed);
    pairs.retain(|(kind, _)| fillable.contains(kind));
    pairs
}

/// The arguments a line spelled out itself, before any sticky filled one in.
fn typed_args(client: &Client, line: &str) -> Option<protocol::Args> {
    if let Some(rest) = line.trim().strip_prefix('/') {
        let (_, tail) = crate::json_command::split(rest);
        if crate::json_command::is_json(&tail) {
            let map = crate::json_command::parse_object(&tail).ok()?;
            return Some(crate::json_command::args_of(&map));
        }
    }
    let Ok(tokens) = classified(line)? else {
        return None;
    };
    let head = tokens.first().cloned().unwrap_or_default();
    let spec = client
        .commands()
        .into_iter()
        .find(|spec| spec.name == head)?;
    crate::commands::parse_args(&tokens[1..], Some(&spec)).ok()
}
