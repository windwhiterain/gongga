//! `new` and `check`: a program a session starts, and what it has said since.
//!
//! See `docs/notes/server-channel.md` and `SPEC.md`, "Tool".

use std::time::Duration;

use serde_json::{Value, json};

use crate::tools::{Cmd, ToolContext, ToolOutcome};

/// What a call that states no `wait` waits for.
///
/// # Invariant
///
/// A default, never a ceiling: `wait: 0` is no wait at all, and the tunnel's clock
/// does not touch these tools -- they are not tunneled, because the shared arguments
/// (`background`, `max_output`, `description`) are what a channel already is.
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub default: Duration,
    /// Output longer than this is capped for the answer, with the whole of it kept in
    /// the home's output directory. `0` disables the cap.
    pub max_output_chars: usize,
}

/// `new`: starts a program as a channel, and reads it up to `until`.
pub struct NewTool {
    pub wait: Wait,
}

impl Cmd for NewTool {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let mut args = args;
            // An operator spells a list or a table as JSON on one line; the model sends
            // the shape itself. Both reach here as whatever the layer could carry.
            if let Some(spelled) = args.get("args").and_then(Value::as_str)
                && let Ok(parsed) = serde_json::from_str::<Value>(spelled)
            {
                args["args"] = parsed;
            }
            let Some(name) = args.get("name").and_then(Value::as_str) else {
                return ToolOutcome::err("`new` needs the `name` of the program to start");
            };
            let name = name.to_string();
            let name = name.as_str();
            // A name the config calls a cmd is answered here, by the command layer, and
            // never becomes a channel: `args` is that cmd's named arguments, with `cmd`
            // naming it.
            if ctx.channels.kind_of(name) == crate::tools::spec::Kind::Cmd {
                return route(ctx, name, &args).await;
            }
            // A list is argv; an object is a cmd's named arguments.
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
                Some(Value::Object(_)) => {
                    return ToolOutcome::err(
                        "`args` given as a table is a cmd's arguments, and this build routes \
                         no name to the command layer yet: pass a list of words, or call the \
                         cmd itself"
                            .to_string(),
                    );
                }
                Some(other) => {
                    return ToolOutcome::err(format!(
                        "`args` is a list of words or a table of named ones, and this is {}",
                        crate::tools::json_type(other)
                    ));
                }
            };
            let row = match ctx
                .channels
                .start(
                    &ctx.session,
                    name,
                    &words,
                    &ctx.cwd,
                    &ctx.env,
                    args.get("id").and_then(Value::as_str).map(str::to_string),
                )
                .await
            {
                Ok(row) => row,
                Err(why) => return ToolOutcome::err(why),
            };
            let input = args.get("input").and_then(Value::as_str);
            let until = args.get("until").and_then(Value::as_str);
            let outcome = ctx
                .channels
                .talk(
                    &ctx.session,
                    &row.path,
                    input,
                    wait_of(&args, self.wait.default),
                    until,
                )
                .await;
            answer(outcome, self.wait.max_output_chars, ctx)
        })
    }
}

/// `check`: speaks to a channel that was started earlier.
pub struct CheckTool {
    pub wait: Wait,
}

impl Cmd for CheckTool {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(id) = args.get("id").and_then(Value::as_str) else {
                return ToolOutcome::err("`check` needs the `id` of the channel to read");
            };
            let outcome = ctx
                .channels
                .talk(
                    &ctx.session,
                    id,
                    args.get("input").and_then(Value::as_str),
                    wait_of(&args, self.wait.default),
                    args.get("until").and_then(Value::as_str),
                )
                .await;
            answer(outcome, self.wait.max_output_chars, ctx)
        })
    }
}

/// Sends one call to the command layer, as `new` with a name the config calls a cmd.
///
/// # Invariant
///
/// The caller's session fills the first argument the target declares as a session, and
/// only when the call did not name one: a command that moves a session elsewhere must
/// not be pinned to the caller's.
async fn route(ctx: &ToolContext, name: &str, args: &Value) -> ToolOutcome {
    let Some(layer) = crate::commands::layer() else {
        return ToolOutcome::err(format!(
            "{name:?} is a cmd, and this process has no command layer: it runs one call and \
             answers with a file"
        ));
    };
    let Some(table) = args.get("args").and_then(Value::as_object) else {
        return ToolOutcome::err(format!(
            "{name:?} is a cmd: `args` is a table of its arguments, with `cmd` naming it, \
             as in {{\"cmd\": \"send_message\", \"message\": \"hi\"}}"
        ));
    };
    let mut words: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (key, value) in table {
        if key == "cmd" {
            continue;
        }
        let text = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        words.insert(key.clone(), text);
    }
    let Some(cmd) = table.get("cmd").and_then(Value::as_str) else {
        return ToolOutcome::err(format!(
            "{name:?} needs `args.cmd`: the command to run, e.g. \"send_message\""
        ));
    };
    let began = std::time::Instant::now();
    let outcome = layer.run(&ctx.session, cmd, words).await;
    let answer = json!({
        "id": Value::Null,
        "output": outcome.message,
        "time": format!("{:.3}s", began.elapsed().as_secs_f64()),
        "matched": false,
        "exit": if outcome.ok { 0 } else { 1 },
    });
    if outcome.ok {
        ToolOutcome::ok(answer.to_string())
    } else {
        ToolOutcome::err(answer.to_string())
    }
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
        Ok(outcome) => {
            let mut answer = outcome.to_json();
            if let Some(window) = answer.get("output").and_then(Value::as_str) {
                let capped = crate::tools::bash::cap_output(
                    crate::prompts::shared(),
                    window,
                    max_output_chars,
                    Some(&ctx.output_dir),
                    &format!("{}", outcome.id),
                );
                answer["output"] = json!(capped);
            }
            ToolOutcome::ok(answer.to_string())
        }
        Err(why) => ToolOutcome::err(why),
    }
}
