//! `exec`: a program a session runs, and what it has said since.
//!
//! See `docs/notes/server-channel.md`.

use std::time::Duration;

use serde_json::Value;

use crate::tools::{Cmd, ToolContext, ToolOutcome};

/// What a call that states no `wait` waits for.
///
/// # Invariant
///
/// A default, never a ceiling: `wait: 0` is no wait at all, and the fault bound is
/// the only thing that ends a call this tool did not ask to end.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(4);
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub default: Duration,
}

/// SPEC's `exec`: starts a program as a channel, or speaks to the one `id` names.
pub struct ExecTool {
    pub wait: Wait,
}

impl Cmd for ExecTool {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let bin = text(&args, "bin");
            let asked = text(&args, "id");
            // A path in a namespace, however it is spelled: Git Bash rewrites a leading
            // `/` into one of this machine's.
            let asked = asked.map(|id| id.trim_start_matches('/').to_string());
            // SPEC's default: `/{name}/default`, named by the program itself.
            let id = match (&bin, &asked) {
                (_, Some(id)) => id.clone(),
                (Some(bin), None) => format!("{}/default", crate::tools::channel::stem(bin)),
                (None, None) => {
                    return ToolOutcome::err(
                        "`exec` needs `bin` to start a program, or the `id` of a channel \
                         already running",
                    );
                }
            };
            let mut running = match ctx.channels.find(&ctx.session, &id) {
                Ok(running) => running,
                Err(why) => return ToolOutcome::err(why),
            };
            // A channel is its program, so a name whose program ended is free.
            if args.get("args").is_some_and(|a| !a.is_null())
                && let Some(row) = running.as_ref()
                && ctx.channels.reap(row).await.is_some()
            {
                running = None;
            }
            let bin = match (bin, &running) {
                // SPEC: a name already speaks for the program it was started with.
                (Some(bin), Some(row))
                    if crate::tools::channel::stem(&row.program)
                        != crate::tools::channel::stem(bin) =>
                {
                    return ToolOutcome::err(format!(
                        "channel {id:?} runs {:?}, not {bin:?}: give the call another `id`, or \
                         `del_channel {id}` first",
                        row.program
                    ));
                }
                (Some(bin), _) => bin.to_string(),
                (None, Some(row)) => row.program.clone(),
                (None, None) => {
                    return ToolOutcome::err(format!(
                        "no channel {id:?} in this session, and no `bin` to start one"
                    ));
                }
            };
            if running.is_some() && args.get("args").is_some_and(|a| !a.is_null()) {
                return ToolOutcome::err(format!(
                    "channel {id:?} is already running {bin:?}: `args` shape a program about \
                     to start, and this call speaks to one that did"
                ));
            }
            // SPEC's `args` is a list of words: one element is one word.
            let words = match args.get("args") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(words)) => {
                    let mut out = Vec::with_capacity(words.len());
                    for word in words {
                        match word.as_str() {
                            Some(word) => out.push(word.to_string()),
                            None => {
                                return ToolOutcome::err(format!(
                                    "`args` is a list of words, and one of them is {}",
                                    crate::tools::json_type(word)
                                ));
                            }
                        }
                    }
                    out
                }
                Some(other) => {
                    return ToolOutcome::err(format!(
                        "`args` is a list of words, and this is {}",
                        crate::tools::json_type(other)
                    ));
                }
            };
            if running.is_none() {
                // Invariant: the machine is settled at creation, from `cwd` alone.
                // See `docs/notes/server-channel.md`.
                let stated_cwd = match protocol::argtype::given(&args, "cwd") {
                    protocol::Given::Value(value) => value.as_str().map(str::trim),
                    _ => None,
                }
                .filter(|cwd| !cwd.is_empty());
                let (remote, cwd, from_session) = match stated_cwd {
                    Some(text) => match crate::space::stated(text) {
                        Ok(stated) => {
                            let peer = stated.peer().map(str::to_string);
                            match stated.directory() {
                                Some(directory) => {
                                    (peer, std::path::PathBuf::from(directory), false)
                                }
                                // The machine and no directory: the machine's own, or the
                                // session's when it is the session's machine.
                                None if peer == ctx.remote && !ctx.cwd.as_os_str().is_empty() => {
                                    (peer, ctx.cwd.clone(), true)
                                }
                                None => (peer, std::path::PathBuf::from("."), false),
                            }
                        }
                        Err(why) => return ToolOutcome::err(why),
                    },
                    // Omitted: the session's own machine and directory, when it has one.
                    None if !ctx.cwd.as_os_str().is_empty() => {
                        (ctx.remote.clone(), ctx.cwd.clone(), true)
                    }
                    None => (ctx.remote.clone(), std::path::PathBuf::from("."), false),
                };
                // Refused by name here: a missing directory is never the program's failure.
                if remote == ctx.remote && !cwd.is_dir() {
                    return ToolOutcome::err(if from_session {
                        format!(
                            "this session's directory {:?} is not on this machine, so this \n                             call has nowhere to start; state a `cwd` for it",
                            cwd.display()
                        )
                    } else {
                        format!(
                            "no directory {:?} on this machine; a call's `cwd` is a \n                             directory that machine has",
                            cwd.display()
                        )
                    });
                }
                if let Err(why) = ctx
                    .channels
                    .start(
                        &ctx.session,
                        &bin,
                        &words,
                        remote.as_deref(),
                        &cwd,
                        &ctx.env,
                        &id,
                    )
                    .await
                {
                    return ToolOutcome::err(why);
                }
            }
            let outcome = ctx
                .channels
                .talk(
                    &ctx.session,
                    &id,
                    args.get("input").and_then(Value::as_str),
                    wait_of(&args, self.wait.default),
                    args.get("until").and_then(Value::as_str),
                    Some(&ctx.cancel),
                    Some(&ctx.inbox),
                )
                .await;
            answer(outcome)
        })
    }
}

/// One argument as a word, when the call gave one that is not blank.
fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|word| !word.is_empty())
}

/// How long this call waits: absent is the default, `0` is not waiting at all.
fn wait_of(args: &Value, default: Duration) -> Duration {
    match args.get("wait").and_then(Value::as_u64) {
        Some(0) => Duration::ZERO,
        Some(millis) => Duration::from_millis(millis),
        None => default,
    }
}

/// What a call answers, as JSON; the call layer caps it, not this tool.
fn answer(outcome: Result<crate::tools::channel::Outcome, String>) -> ToolOutcome {
    match outcome {
        Ok(outcome) => ToolOutcome::ok(outcome.to_json().to_string()),
        Err(why) => ToolOutcome::err(why),
    }
}
