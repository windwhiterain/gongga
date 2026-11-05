//! `exec`: run one program with its arguments, without a shell.
//!
//! # Invariant
//!
//! No shell reads the command. The program is spawned with the words it was
//! given, so a word that holds a space, a quote, a `;` or a `$` reaches the
//! program as itself. Nothing here interprets anything, so nothing here can be
//! made to.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::exec_remote;
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// What `exec` says it is: one program, its arguments, no shell.
const EXEC_DESCRIPTION: &str = "Run one program with its arguments, without a shell. \
     The command is a list of words: the program first, then its arguments, one \
     word each. No shell reads them, so a word that holds a space, a quote, a \
     `;` or a `$` reaches the program exactly as written -- there is nothing to \
     escape and nothing to escape it wrongly. What a shell would do (pipes, \
     redirects, `cd`, `&&`, variable expansion, wildcards) is not available \
     here; use `bash` for those. The program is looked up on PATH, and it runs \
     in the session's directory. This is not the persistent shell: `cd` and \
     exports do not survive the call.";

/// What `remote_exec` says it is: the same thing, on the session's peer.
const REMOTE_EXEC_DESCRIPTION: &str = "`exec` on the session's remote endpoint -- \
     the program and its arguments, with no shell between them. Same command, \
     same absence of a shell, and the program is looked up on the peer's PATH. \
     The directory is the one the peer reports, and it is passed as it is: a \
     path this machine holds is not evidence about one the peer has.";

pub struct ExecTool {
    pub machine: Machine,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Tool for ExecTool {
    fn name(&self) -> &'static str {
        match self.machine {
            Machine::Local => "exec",
            Machine::Remote => "remote_exec",
        }
    }

    fn description(&self) -> &'static str {
        match self.machine {
            Machine::Local => EXEC_DESCRIPTION,
            Machine::Remote => REMOTE_EXEC_DESCRIPTION,
        }
    }

    fn machine(&self, _args: &Value, _ctx: &ToolContext) -> Machine {
        self.machine
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The program and its arguments, one word each: the \
                                     program first. No word is interpreted, so a word \
                                     that holds a space, a quote, a `;` or a `$` needs \
                                     no escaping and gets none."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- it is read \
                                     right after the tool's name, so say what the run \
                                     is for, not what the command is (`check the state`, \
                                     not `exec`). Optional, and never executed: it also \
                                     names the file that a too-long output is spilled \
                                     to, so that later you (and whoever reads the dumps) \
                                     can tell one run from another."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "How long to wait for the program before giving up \
                                     on it. When it passes, the program is stopped and \
                                     what it printed is what this call answers with."
                }
            },
            "required": ["command"]
        })
    }

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
                    super::words::run(program, &command.args, &command.cwd, &ctx.env, timeout).await
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
