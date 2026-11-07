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
            let Some(name) = args.get("name").and_then(Value::as_str) else {
                return ToolOutcome::err("`new` needs the `name` of the program to start");
            };
            // A list is argv; an object is a cmd's named arguments, and no name is
            // routed to the command layer yet.
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
