//! `exec`: run one program with its arguments, without a shell.
//!
//! # Invariant
//!
//! No shell reads the command. The program is spawned with the words it was
//! given, so a word that holds a space, a quote, a `;` or a `$` reaches the
//! program as itself. Nothing here interprets anything, so nothing here can be
//! made to.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::exec_remote;
use crate::tools::{Cut, Tool, ToolContext, ToolOutcome};

pub struct ExecTool {
    pub machine: Machine,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Cmd for ExecTool {
    /// A program run this way has no shell to be left in.
    ///
    /// # Invariant
    ///
    /// The wait is dropped with the call. A local program is a process of its own, so a
    /// stop that lets jobs go leaves it running and the row carries it; every other cut
    /// ends it, and what is left to say is that it was stopped rather than finished.
    /// See `docs/notes/server-tools-exec.md`.
    fn cut<'a>(
        &'a self,
        args: &'a Value,
        ctx: &'a ToolContext,
        why: Cut,
        _started: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            // Named only if the words could be read: a cut is not where a bad
            // argument is reported.
            let program = match command_of(args, ctx) {
                Ok(command) => format!("`{}`", command.program),
                Err(_) => "the program".to_string(),
            };
            if crate::commands::cancel_lets_go()
                && let Ok(Some(id)) = ctx.store.job_of_call(&ctx.session, &ctx.call_id)
            {
                return ToolOutcome::err(format!(
                    "{program} was still running: {why}. This stop lets background jobs go, so \
                     it runs on in a process of its own as job {id}: `check_job` with `job_id: \
                     {id}` collects what it prints, and the next start adopts it.",
                    why = why.because()
                ));
            }
            ToolOutcome::err(format!(
                "{program} was stopped: {why}. A program started with `{}` has no shell to be \
                 left running in, so it ends with the call -- what it printed after this point \
                 is lost, and its exit status is unknown.",
                self.name(),
                why = why.because()
            ))
        })
    }

    /// The program's own clock ends the wait by handing it to the job table.
    ///
    /// # Invariant
    ///
    /// A local call that waits: the tunnel's clock would otherwise fire at the same
    /// instant and record a second job for one call. A peer's program has no row this
    /// server could adopt, so it keeps the tunnel's clock and ends with the call.
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let command = match command_of(&args, ctx) {
                Ok(command) => command,
                Err(why) => return ToolOutcome::err(why),
            };
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let timeout = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(self.timeout);

            let outcome = match self.machine {
                Machine::Local => {
                    let program = std::path::Path::new(&command.program);
                    // The session's own facts, as its shell has them: a program that
                    // asks `$GONGGA_SESSION` is asking about this session.
                    if ctx.holds_the_turn {
                        // A process of its own, so a wait that ends does not end it.
                        match super::tunnel::run_own_program(
                            ctx,
                            self.name(),
                            description,
                            self.max_output_chars,
                            program,
                            &command.args,
                            &command.cwd,
                            &ctx.env,
                            timeout,
                        )
                        .await
                        {
                            Ok(outcome) => Ok(outcome),
                            Err(answered) => return answered,
                        }
                    } else {
                        // A worker answers for its own call and has nothing to hand the
                        // work to, so its program ends with the wait.
                        super::words::run(program, &command.args, &command.cwd, &ctx.env, timeout)
                            .await
                    }
                }
                Machine::Remote => {
                    // The peer runs it: the program is looked up on its PATH and
                    // started with the words, in its own directory.
                    let Some(endpoint) = ctx.endpoint(Machine::Remote) else {
                        return ToolOutcome::err(
                            "exec ran nothing: this session names no remote, so it has no peer to \
                             run on"
                                .to_string(),
                        );
                    };
                    let at = ctx.dir(Machine::Remote);
                    let at = at.to_string_lossy().into_owned();
                    match exec_remote::run(&endpoint, &command.program, &command.args, &at, timeout)
                        .await
                    {
                        Ok(outcome) => Ok(super::words::RunOutcome {
                            stdout: outcome.stdout,
                            stderr: outcome.stderr,
                            code: outcome.code,
                        }),
                        Err(why) => Err(why),
                    }
                }
            };
            match outcome {
                Ok(super::words::RunOutcome {
                    stdout,
                    stderr,
                    code,
                }) => {
                    let mut text = stdout;
                    if !stderr.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&stderr);
                    }
                    let text = ctx.cap(
                        self.machine,
                        "exec",
                        description,
                        text,
                        self.max_output_chars,
                    );
                    let report = if text.is_empty() {
                        format!("[exit status {code}]")
                    } else {
                        format!("{text}\n[exit status {code}]")
                    };
                    ToolOutcome {
                        output: report,
                        ok: code == 0,
                    }
                }
                Err(why) => ToolOutcome::err(why),
            }
        })
    }

    /// A program run this way has no shell to be left in.
    ///
    /// # Invariant
    ///
    /// The wait is dropped with the call. A local program is a process of its own, so a
    /// stop that lets jobs go leaves it running and the row carries it; every other cut
    /// ends it, and what is left to say is that it was stopped rather than finished.
    /// See `docs/notes/server-tools-exec.md`.
}
}

fn command_of(args: &Value, ctx: &ToolContext) -> Result<ExecCommand, String> {
    let Some(words) = args.get("command").and_then(Value::as_array) else {
        return Err("exec requires a `command`: a list of words, the program first".to_string());
    };
    if words.is_empty() {
        return Err("exec received an empty list of words".to_string());
    }
    let mut named: Vec<&str> = Vec::with_capacity(words.len());
    for word in words {
        let Some(word) = word.as_str() else {
            return Err(format!(
                "exec received a list whose words must all be strings, and one was {kind}",
                kind = crate::tools::json_type(word),
            ));
        };
        // A NUL ends an argument for every program there is.
        if word.contains('\0') {
            return Err(
                "exec received a word holding a NUL byte, which no program could receive: \
                 an argument ends at a NUL, so it would arrive truncated"
                    .to_string(),
            );
        }
        named.push(word);
    }
    let mut named = named.into_iter();
    let program = named.next().expect("the list is not empty").to_string();
    let args: Vec<String> = named.map(str::to_string).collect();
    Ok(ExecCommand {
        program,
        args,
        cwd: ctx.cwd.clone(),
    })
}

/// The program and its arguments, as the caller gave them.
pub struct ExecCommand {
    pub program: String,
    pub args: Vec<String>,
    /// The directory the program runs in.
    pub cwd: PathBuf,
}
