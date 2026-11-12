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
/// A default, never a ceiling: `wait: 0` is no wait at all, and the tunnel's clock
/// does not touch this tool -- it is not tunneled, because the shared arguments
/// (`background`, `max_output`, `description`) are what a channel already is.
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub default: Duration,
    /// Output longer than this is capped, the whole of it kept on disk. `0` is no cap.
    pub max_output_chars: usize,
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
            let args = spelled(args);
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
            // A list is argv; an object is named arguments, each spoken as `--key value`.
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
                Some(Value::Object(named)) => {
                    let mut out = Vec::with_capacity(named.len() * 2);
                    for (key, value) in named {
                        match value.as_str() {
                            Some(value) => {
                                out.push(format!("--{key}"));
                                out.push(value.to_string());
                            }
                            None => {
                                return ToolOutcome::err(format!(
                                    "`args` named arguments take string values, and {key:?} is {}",
                                    crate::tools::json_type(value)
                                ));
                            }
                        }
                    }
                    out
                }
                Some(other) => {
                    return ToolOutcome::err(format!(
                        "`args` is a list of words or a table of named ones, and this is {}",
                        crate::tools::json_type(other)
                    ));
                }
            };
            if running.is_none() {
                // The machine and the directory are the call's; a session's are defaults.
                let remote = match args.get("remote").and_then(Value::as_str) {
                    Some(name) if name.is_empty() => None,
                    Some(name) => Some(name.to_string()),
                    None => ctx.remote.clone(),
                };
                let cwd = match args.get("cwd").and_then(Value::as_str) {
                    Some(path) if !path.is_empty() => std::path::PathBuf::from(path),
                    // The session's directory only when the call asked for its machine.
                    _ if remote == ctx.remote => ctx.cwd.clone(),
                    _ => std::path::PathBuf::from("."),
                };
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
                )
                .await;
            answer(outcome, self.wait.max_output_chars, ctx)
        })
    }
}

/// The call's arguments, with the two spellings of `args` read as what they mean.
///
/// # Invariant
///
/// An operator spells a list or a table as JSON on one line, and a one-element list
/// holding one is that one: SPEC's `args` is `list[str] | dict[str]`.
fn spelled(mut args: Value) -> Value {
    if let Some(spelled) = args.get("args").and_then(Value::as_str)
        && let Ok(parsed) = serde_json::from_str::<Value>(spelled)
    {
        args["args"] = parsed;
    }
    if let Some(Value::Array(one)) = args.get("args")
        && one.len() == 1
    {
        let only = match &one[0] {
            Value::Object(_) => Some(one[0].clone()),
            Value::String(spelled) => serde_json::from_str::<Value>(spelled)
                .ok()
                .filter(Value::is_object),
            _ => None,
        };
        if let Some(table) = only {
            args["args"] = table;
        }
    }
    args
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

/// What a call answers: the window as JSON, capped like every other tool's output.
fn answer(
    outcome: Result<crate::tools::channel::Outcome, String>,
    max_output_chars: usize,
    ctx: &ToolContext,
) -> ToolOutcome {
    match outcome {
        Ok(outcome) => ToolOutcome::ok(
            outcome
                .result(max_output_chars, &ctx.output_dir)
                .to_string(),
        ),
        Err(why) => ToolOutcome::err(why),
    }
}
