//! The tool set exposed to the model.
//!
//! The SPEC gives the model `bash{name, cmd}` to run a command in a named
//! persistent shell and `del_bash{name}` to throw one away; `check_bash` and
//! `list_bash` are the reading half of the same idea, and `kill_bash` exists
//! because a `detached` job outlives everything else and therefore needs an
//! explicit way to be stopped. Shells are keyed `session_name/bash_name`, so
//! two sessions never share one and a single session can keep several
//! independent ones.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use protocol::{ToolAnswerEvent, ToolStream};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::session::{DEFAULT_BASH, ShellSet};
use crate::store::Store;
use crate::tools::bash::{BashChunk, BashShell, BashStream};
use crate::tools::detached::DetachedRegistry;

pub mod bash;
pub mod detached;
#[cfg(windows)]
pub mod job;

/// Everything a tool invocation needs, supplied per call by the agent loop.
pub struct ToolContext {
    pub session: String,
    pub call_id: String,
    /// Where capped tool output is spilled, so nothing is lost by capping.
    pub output_dir: PathBuf,
    /// The session's named shells.
    pub shells: Arc<StdMutex<ShellSet>>,
    /// Detached jobs, keyed by session. Deliberately not part of a shell: a
    /// detached job outlives its shell, its session and the server.
    pub detached: DetachedRegistry,
    /// Streaming sink for `tool_answer` events.
    pub events: Option<mpsc::Sender<ToolAnswerEvent>>,
    pub cancel: CancellationToken,
    /// Where a command that outlived its deadline reports back to the session
    /// when it finishes. That report is an inbox row, not a tool result: the
    /// call it belongs to returned long ago, and the session may be idle by
    /// the time the command ends.
    pub store: Store,
}

impl ToolContext {
    /// Emits one streamed chunk, or drops it if nobody is listening.
    pub async fn emit(&self, stream: ToolStream, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(tx) = &self.events {
            let _ = tx
                .send(ToolAnswerEvent {
                    session: self.session.clone(),
                    call_id: self.call_id.clone(),
                    // The tool name is filled in by the loop, which owns it.
                    tool: String::new(),
                    stream,
                    text,
                    done: false,
                })
                .await;
        }
    }

    /// The handle for `name`, spawning the shell if this is its first use.
    fn shell(&self, name: &str) -> anyhow::Result<Arc<Mutex<BashShell>>> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
    }

    /// The handle without spawning, for tools that must not create one
    /// (`check_bash`, `list_bash`).
    fn peek(&self, name: &str) -> Option<Arc<Mutex<BashShell>>> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(name)
    }

    /// The shells that exist right now.
    fn shell_names(&self) -> Vec<String> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .names()
    }

    fn forget(&self, name: &str) -> bool {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
    }

    /// Retires `handle` and puts a freshly spawned shell under `name`.
    ///
    /// Every path that ends a shell for a reason the model did not ask for --
    /// a command that outlived its wait, output that lost its sentinel, a turn
    /// cancelled mid-command -- does the same thing afterwards: the name has to
    /// keep meaning "a shell you can use", because the tool results and
    /// `list_bash` say it does. The pointer check inside `replace` is what keeps
    /// this from taking away a shell somebody else already put there; `false`
    /// means the name now belongs to a different shell.
    fn replace_shell(&self, name: &str, handle: &Arc<Mutex<BashShell>>) -> bool {
        match self
            .shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(name, handle)
        {
            Ok(replaced) => replaced,
            Err(e) => {
                tracing::warn!(
                    session = %self.session,
                    shell = %name,
                    "could not put a fresh shell in place: {e}"
                );
                false
            }
        }
    }

    /// Caps one command's output, spilling the rest to a file named after
    /// where it came from: session, shell, `task`, call id.
    ///
    /// The `task` is in there because a dump is read later, out of context --
    /// a dozen `...-default-call_00_abc.out` files say nothing, and the whole
    /// point of keeping the full output is being able to find the right one
    /// again. `slug()` in `bash.rs` is what makes it safe for a path.
    fn cap(&self, shell: &str, task: Option<&str>, text: String, max: usize) -> String {
        let label = spill_label(&self.session, shell, task, &self.call_id);
        crate::tools::bash::cap_output(&text, max, &self.output_dir, &label)
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    /// Text handed back to the model as the function call output.
    pub output: String,
    pub ok: bool,
}

impl ToolOutcome {
    pub fn ok(output: impl Into<String>) -> Self {
        Self { output: output.into(), ok: true }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self { output: output.into(), ok: false }
    }
}

/// A tool the model can call.
///
/// Implemented with a boxed future rather than `async_trait` so the crate keeps
/// one fewer macro dependency; the signature is the same either way.
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters(&self) -> Value;

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn with_builtins(bash_timeout: Duration, max_output_chars: usize) -> Self {
        Self {
            tools: vec![
                Box::new(BashTool {
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Box::new(CheckBashTool { max_output_chars }),
                Box::new(DelBashTool),
                Box::new(ListBashTool),
                Box::new(KillBashTool),
            ],
        }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    /// Tool schemas in the shape the Responses API expects.
    pub fn definitions(&self) -> Vec<crate::llm::ToolDefinition> {
        self.tools
            .iter()
            .map(|t| crate::llm::ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
    }

    pub async fn run(&self, name: &str, args: Value, ctx: &ToolContext) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => match argument_error(tool, &args) {
                Some(message) => ToolOutcome::err(message),
                None => tool.run(args, ctx).await,
            },
            None => ToolOutcome::err(format!(
                "unknown tool {name:?}; available: {}",
                self.tools
                    .iter()
                    .map(|t| t.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}

/// Why a call does not fit a tool's declared schema, in the schema's own words.
///
/// Assembled from [`Tool::parameters`] rather than written per tool, so it
/// cannot drift from what the model was handed: which arguments exist, their
/// types, which are required, and which declared name a wrong one was probably
/// reaching for. A hint that has to be maintained by hand is a hint that goes
/// stale the first time an argument is renamed.
///
/// Only a *missing required* argument rejects the call. A stray key on its own
/// is passed through, exactly as it was before this existed: a call that would
/// have run is a call the operator wanted, and failing it over an ignored key
/// would manufacture the retries this message exists to prevent.
fn argument_error(tool: &dyn Tool, args: &Value) -> Option<String> {
    let schema = tool.parameters();
    let Some(given) = args.as_object() else {
        return Some(format!(
            "`{name}` did not run: its arguments must be a JSON object, and this was {kind}.\n  \
             `{name}` takes: {signature}",
            name = tool.name(),
            kind = json_type(args),
            signature = signature(&schema),
        ));
    };

    let required = string_list(&schema["required"]);
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| given.get(*k).is_none_or(Value::is_null))
        .collect();
    if missing.is_empty() {
        return None;
    }

    let mut message = format!(
        "`{}` did not run: {} {} required.\n",
        tool.name(),
        missing
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", "),
        if missing.len() == 1 { "is" } else { "are" },
    );

    let declared = string_list(&schema["properties"]);
    let unexpected: Vec<String> = given
        .keys()
        .filter(|k| !declared.contains(&k.as_str()))
        .map(|wrong| match declared.iter().find(|d| is_shorthand_of(d, wrong)) {
            Some(right) => format!("`{wrong}` (did you mean `{right}`?)"),
            None => format!("`{wrong}`"),
        })
        .collect();
    if !unexpected.is_empty() {
        message.push_str(&format!("  unexpected: {}\n", unexpected.join(", ")));
    }
    message.push_str(&format!(
        "  `{}` takes: {}",
        tool.name(),
        signature(&schema)
    ));
    Some(message)
}

/// The tool's whole argument list, one line: name, type, required or not.
fn signature(schema: &Value) -> String {
    let required = string_list(&schema["required"]);
    let Some(properties) = schema["properties"].as_object() else {
        return "no arguments".to_string();
    };
    properties
        .iter()
        .map(|(name, spec)| {
            let kind = spec["type"].as_str().unwrap_or("any");
            let needed = if required.contains(&name.as_str()) {
                "required"
            } else {
                "optional"
            };
            format!("{name}: {kind} ({needed})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The strings of a JSON array, or the keys of an object, in document order.
fn string_list(value: &Value) -> Vec<&str> {
    match value {
        Value::Array(items) => items.iter().filter_map(|v| v.as_str()).collect(),
        Value::Object(map) => map.keys().map(String::as_str).collect(),
        _ => Vec::new(),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Is one of these two names spelled out inside the other, in order?
///
/// `cmd` is inside `command` and `command` is inside `cmdline`, so direction
/// does not matter: a name the model invents for an argument is nearly always
/// a shortening or a spelling-out of the declared one, and naming the declared
/// one back is what turns a rejected call into a corrected one. This is a rule
/// over the schema, not a list of synonyms, so a renamed argument never needs a
/// new entry -- and a key unrelated to anything declared gets no guess rather
/// than a wrong one.
fn is_shorthand_of(declared: &str, given: &str) -> bool {
    let (short, long) = if declared.chars().count() <= given.chars().count() {
        (declared, given)
    } else {
        (given, declared)
    };
    if short.chars().count() < 2 || short == long {
        return false;
    }
    let mut spelled = long.chars();
    short.chars().all(|c| spelled.any(|l| l == c))
}

/// The turn was cancelled while a command was running.
///
/// Its own type rather than a string, because the cancel path has work to do
/// that no other error does: the command has to be stopped, and the shell it
/// was running in has to be thrown away. A message-shaped error would make
/// that work a string comparison.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

pub struct BashTool {
    pub timeout: Duration,
    /// 0 disables the cap.
    pub max_output_chars: usize,
}

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in a persistent Git Bash. Each `name` is a \
         separate shell that keeps its own working directory, exports and shell \
         functions between calls, and stays alive until you `del_bash` it; omit \
         `name` to use `default`. So reusing one name is how state accumulates -- \
         inventing a name per command leaves one idle process behind per command. \
         Output is streamed as it is produced. Use `cd` to move around instead of \
         passing absolute paths every time. Very long output is capped: you get \
         the beginning and the end, and the full text is written to a file named \
         in the result -- read that file with `sed -n` or `grep`, do not cat it. \
         With `background`, the command is started and the call returns \
         immediately; collect what it printed later with `check_bash`."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Which persistent shell to use. Defaults to `default`. \
                                     One name is one live process that stays alive until \
                                     `del_bash`: reusing a name is what carries `cd`, \
                                     exports and shell functions from one call to the next, \
                                     and a fresh name for every call starts a fresh process \
                                     every call. Unless you need two shells side by side, \
                                     use one name throughout and put what the call is for in \
                                     `task`."
                },
                "command": {
                    "type": "string",
                    "description": "The command line to execute."
                },
                "task": {
                    "type": "string",
                    "description": "What this call is for, in a few words. Optional, and \
                                     never executed: it only names the file that a too-long \
                                     output is spilled to, so that later you (and whoever \
                                     reads the dumps) can tell one run from another. \
                                     Example: `run the server tests`."
                },
                "background": {
                    "type": "boolean",
                    "description": "Start the command and return immediately instead of \
                                     waiting for it. Defaults to false."
                },
                "detached": {
                    "type": "boolean",
                    "description": "Run the command as a process of its own and return at \
                                     once. It keeps running when the shell is discarded, \
                                     when the session is cancelled and even after the server \
                                     exits -- that is how you restart the server you are \
                                     running on. Nothing is collected: its output is written \
                                     to the log file named in the result. Stop it later with \
                                     `kill_bash`. Defaults to false."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "How long to wait for the command before returning. \
                                     This is a limit on waiting, not on the command: when \
                                     it passes, the command keeps running in the background \
                                     and its output is posted to this session when it \
                                     finishes. Ignored for background commands."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            // The argument is `command`, not `cmd`: a model reaching for the
            // word it already knows is right far more often than it is wrong,
            // and the schema should be the thing that bends. A missing one is
            // rejected by `argument_error` first, in the schema's own words;
            // what is left here is a `command` that is not a string.
            let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                return ToolOutcome::err("bash requires a string `command`");
            };
            if command.trim().is_empty() {
                return ToolOutcome::err("bash received an empty command");
            }
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_BASH);
            // Only ever a label: it names the spill file and nothing else, so a
            // missing or nonsensical one costs a less informative path rather
            // than a failed call.
            let task = args
                .get("task")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            // Both flags return at once, which is the point: the turn stays
            // responsive instead of sitting out a long command. They differ in
            // what happens to the output, not in whether the call blocks.
            let background = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
            let detached = args.get("detached").and_then(|v| v.as_bool()).unwrap_or(false);
            if background && detached {
                return ToolOutcome::err(
                    "pick one: `background` collects the output, `detached` does not",
                );
            }
            if background || detached {
                let shell = match ctx.shell(name) {
                    Ok(s) => s,
                    Err(e) => return ToolOutcome::err(format!("{e}")),
                };
                if background {
                    let mut shell = shell.lock().await;
                    return match shell.run_background(command).await {
                        Ok(job) => ToolOutcome::ok(format!(
                            "started background job {job} in shell {name}; \
                             run `check_bash` to collect its output"
                        )),
                        Err(e) => ToolOutcome::err(format!("{e}")),
                    };
                }
                // Only the working directory is inherited. Exports and shell
                // functions are not, because a detached job does not run in the
                // shell -- it is a process of its own.
                let cwd = {
                    let shell = shell.lock().await;
                    shell.cwd().to_path_buf()
                };
                return match ctx.detached.start(&ctx.session, name, command, &cwd).await {
                    Ok(job) => ToolOutcome::ok(format!(
                        "started detached job {} (pid {}) from shell {name}. It outlives the \
                         shell, the session, `cancel` and even a server restart, so it is how \
                         you replace the server you are running on; only `kill_bash` stops it. \
                         Its output is not collected -- read it at: {}",
                        job.job, job.pid, job.log
                    )),
                    Err(e) => ToolOutcome::err(format!("{e}")),
                };
            }

            let timeout = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .map(Duration::from_millis)
                .unwrap_or(self.timeout)
                .max(Duration::from_millis(100));

            let handle = match ctx.shell(name) {
                Ok(s) => s,
                Err(e) => return ToolOutcome::err(format!("{e}")),
            };

            // Relay shell output to `tool_answer` events while the command runs.
            // Draining has to be concurrent with the run itself: if we awaited
            // the command first, a chatty command would fill the channel and
            // block the shell.
            let (tx, mut rx) = mpsc::channel::<BashChunk>(256);
            let mut shell = handle.lock().await;
            // Scoped so the future borrowing `shell` is dropped before the guard
            // is, which is what lets the guard go out of scope at all.
            let outcome = {
                let execute = shell.run(command, timeout, Some(&tx));
                tokio::pin!(execute);
                loop {
                    tokio::select! {
                        res = &mut execute => break res,
                        _ = ctx.cancel.cancelled() => break Err(anyhow::Error::new(Cancelled)),
                        maybe = rx.recv() => {
                            if let Some(chunk) = maybe {
                                let stream = match chunk.stream {
                                    BashStream::Stdout => ToolStream::Stdout,
                                    BashStream::Stderr => ToolStream::Stderr,
                                };
                                ctx.emit(stream, chunk.text).await;
                            }
                        }
                    }
                }
            };
            // A cancelled command is stopped, not abandoned. Dropping the
            // future only stops us reading: the shell keeps the command, and
            // would answer the *next* call with its leftovers (and with a
            // success status that belongs to it). Killing the process group is
            // both the "turn cancel interrupts the foreground command" contract
            // and the only way to know the shell has no command boundary owed.
            let cancelled = matches!(&outcome, Err(e) if e.is::<Cancelled>());
            if cancelled {
                shell.kill().await;
            }
            // Read while the guard is still held: a shell that lost its output
            // stream must not be handed another command, and this is the last
            // moment its state is visible without taking the lock again.
            let healthy = shell.usable();
            drop(shell);
            while let Ok(chunk) = rx.try_recv() {
                let stream = match chunk.stream {
                    BashStream::Stdout => ToolStream::Stdout,
                    BashStream::Stderr => ToolStream::Stderr,
                };
                ctx.emit(stream, chunk.text).await;
            }

            match outcome {
                // The deadline was on waiting, not on the command. Nothing was
                // stopped: the shell still holds it. Give the session a fresh
                // shell under the same name so work can continue, and have the
                // old one report the rest of the output into the inbox.
                Ok(o) if o.still_running => {
                    let retired = ctx.replace_shell(name, &handle);
                    if !retired {
                        tracing::warn!(
                            session = %ctx.session,
                            shell = %name,
                            "shell was replaced while its command was still running"
                        );
                    }
                    let store = ctx.store.clone();
                    let session = ctx.session.clone();
                    let output_dir = ctx.output_dir.clone();
                    // Same naming rule as the foreground path, with `late`
                    // standing in for the call id: the call it belonged to has
                    // already returned, so its id is no longer what identifies
                    // the run.
                    let label = spill_label(&session, name, task, "late");
                    let cap = self.max_output_chars;
                    let waited = timeout;
                    let owned = name.to_string();
                    tokio::spawn(async move {
                        // Nobody else holds this shell any more, so waiting for
                        // the lock is waiting for the call that timed out to
                        // return -- which has already happened.
                        let (stdout, stderr, code) = {
                            let mut shell = handle.lock().await;
                            shell.finish_outstanding().await
                        };
                        let mut text = stdout;
                        if !stderr.is_empty() {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&stderr);
                        }
                        let text = crate::tools::bash::cap_output(
                            &text,
                            cap,
                            &output_dir,
                            &label,
                        );
                        let text = if text.is_empty() {
                            "(no output)".to_string()
                        } else {
                            text
                        };
                        let status = match code {
                            Some(0) => "finished successfully".to_string(),
                            Some(c) => format!("finished with exit status {c}"),
                            None => "finished, though the exit status could not be read"
                                .to_string(),
                        };
                        let message = format!(
                            "The command you started in shell `{owned}`, which outlived its \
                             {secs}s wait and was left running, has now {status}.\n\n{text}",
                            secs = waited.as_secs()
                        );
                        if let Err(e) = store.add_pending_task(
                            &session,
                            "inject",
                            &json!({"message": message}),
                        ) {
                            tracing::warn!(session = %session, "could not queue a finished command: {e}");
                        }
                    });
                    ToolOutcome::ok(format!(
                        "still running after {}s, so the wait ended -- the command was not \
                         stopped. It continues in the old shell `{name}`, which is now detached \
                         from this session; its output will be posted to this session when it \
                         finishes. `{name}` is a fresh shell now, in the session's working \
                         directory.{}",
                        timeout.as_secs(),
                        if retired {
                            ""
                        } else {
                            " (Another shell had already taken that name, so the old one is \
                              only reachable through its completion message.)"
                        }
                    ))
                }
                Ok(o) => {
                    let mut text = String::new();
                    if !o.stdout.is_empty() {
                        text.push_str(&o.stdout);
                    }
                    if !o.stderr.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&o.stderr);
                    }
                    if !healthy {
                        // The pipe ended without its sentinel, so this shell
                        // cannot tell one command's output from another's. It
                        // is replaced here rather than on the next call, so the
                        // model never gets to send a command to a shell that
                        // would answer it with the previous command's text.
                        let replaced = ctx.replace_shell(name, &handle);
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&format!(
                            "[shell `{name}` lost the end of this command's output: a stream \
                             closed without its sentinel, which `exit`, a kill, or a crashed \
                             child can cause. {}. Anything above may be truncated.]",
                            if replaced {
                                "It has been replaced with a fresh shell in the session's \
                                 working directory, so `cd` and exports from earlier calls are \
                                 gone"
                            } else {
                                "A fresh shell could not be put in place, so use another name \
                                 or `del_bash` it"
                            }
                        ));
                    }
                    // Capped before it is stored: what goes into the transcript
                    // is replayed on every later step, so the full output of a
                    // chatty command would be paid for over and over.
                    let text = ctx.cap(name, task, text, self.max_output_chars);
                    let text = if text.is_empty() {
                        "(no output)".to_string()
                    } else {
                        text
                    };
                    match o.exit_code {
                        Some(0) => ToolOutcome::ok(text),
                        Some(code) => {
                            ToolOutcome::err(format!("{text}\n[exit status {code}]"))
                        }
                        None => ToolOutcome::err(format!("{text}\n[shell exited]")),
                    }
                }
                Err(e) if e.is::<Cancelled>() => {
                    // Same treatment as lost output: the shell may still be
                    // holding part of a command that was stopped mid-flight, so
                    // it is never handed another one. The result says what
                    // happened instead of pretending the command finished --
                    // the model must not read a cancelled command's silence as
                    // success, and it must not expect its output.
                    let replaced = ctx.replace_shell(name, &handle);
                    ToolOutcome::err(format!(
                        "cancelled: the turn was interrupted and this command was stopped. \
                         What it printed above (if anything) is all there is, and its exit \
                         status is unknown. {}",
                        if replaced {
                            format!(
                                "Shell `{name}` was replaced with a fresh one in the session's \
                                 working directory, so `cd` and exports from earlier calls are \
                                 gone."
                            )
                        } else {
                            format!(
                                "A fresh shell could not be put in place under `{name}`, so use \
                                 another name or `del_bash` it."
                            )
                        }
                    ))
                }
                Err(e) => {
                    // The write itself failed, which can leave the shell with
                    // half a command line in its stdin. `healthy` is what says
                    // so; dropping it here is how the next call ends up reading
                    // this one's leftovers.
                    if !healthy {
                        ctx.replace_shell(name, &handle);
                    }
                    ToolOutcome::err(format!("{e}"))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// check_bash
// ---------------------------------------------------------------------------

pub struct CheckBashTool {
    pub max_output_chars: usize,
}

impl Tool for CheckBashTool {
    fn name(&self) -> &'static str {
        "check_bash"
    }

    fn description(&self) -> &'static str {
        "Collect the output a shell has produced but you have not seen yet: \
         everything printed by commands started with `background`, plus \
         anything the shell emitted while nobody was reading. Reading it \
         clears it, so each chunk is returned exactly once. Empty output means \
         the job has not printed anything yet -- wait and ask again."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Shell to read. Defaults to `default`."
                }
            },
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let name = shell_name(&args);
            let Some(handle) = ctx.peek(name) else {
                return ToolOutcome::err(format!("no live shell named {name}"));
            };
            let mut shell = handle.lock().await;
            let text = ctx.cap(name, None, shell.take_pending(), self.max_output_chars);
            let jobs = shell.background_jobs();
            if text.is_empty() {
                return ToolOutcome::ok(if jobs == 0 {
                    format!("{name}: no unread output")
                } else {
                    format!("{name}: {jobs} job(s) still running, no output yet")
                });
            }
            ToolOutcome::ok(format!(
                "{text}\n[{name}: {} job(s) still running]",
                jobs
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// list_bash
// ---------------------------------------------------------------------------

pub struct ListBashTool;

impl Tool for ListBashTool {
    fn name(&self) -> &'static str {
        "list_bash"
    }

    fn description(&self) -> &'static str {
        "List the persistent shells of this session: their names, working \
         directories, how much unread output they are holding, and how many \
         background jobs are still running. Also lists the session's detached \
         jobs, which are not any shell's and survive them all."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        _args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let mut rows = Vec::new();
            for name in ctx.shell_names() {
                let Some(handle) = ctx.peek(&name) else {
                    continue;
                };
                let shell = handle.lock().await;
                rows.push(json!({
                    "name": name,
                    "cwd": shell.cwd().display().to_string(),
                    "unread_bytes": shell.pending_len(),
                    "background_jobs": shell.background_jobs(),
                }));
            }
            // Detached jobs belong to the session, not to a shell, so they are
            // listed on their own -- including ones adopted from a previous run
            // of the server, whose shell no longer exists.
            let detached: Vec<Value> = ctx
                .detached
                .list(&ctx.session)
                .await
                .iter()
                .map(|j| j.to_json())
                .collect();
            if rows.is_empty() && detached.is_empty() {
                return ToolOutcome::ok("this session has no shells and no detached jobs");
            }
            ToolOutcome::ok(
                serde_json::to_string_pretty(&json!({"bash": rows, "detached": detached}))
                    .unwrap_or_default(),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// del_bash
// ---------------------------------------------------------------------------

pub struct DelBashTool;

impl Tool for DelBashTool {
    fn name(&self) -> &'static str {
        "del_bash"
    }

    fn description(&self) -> &'static str {
        "Discard persistent shells whose name matches a regex. Their working \
         directories, exports and shell functions are lost; naming one again in \
         `bash` starts a fresh shell in the session's directory. Background jobs \
         go with them; detached jobs do not, because they were never the \
         shells'. Use this when a shell is wedged or polluted, and to clean up \
         the names a long session invented."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name_regex": {
                    "type": "string",
                    "description": "Pattern matched against shell names, unanchored: \
                                    `qa` also takes `qa2`. Defaults to `^default$`."
                }
            },
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            // Unanchored on purpose: `qa` is how someone actually names a shell
            // family (`qa`, `qa2`, `qa5`), and requiring `^qa` would make the
            // cleanup of a long session's leftovers guesswork.
            let pattern = args
                .get("name_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("^default$");
            let Ok(regex) = regex::Regex::new(pattern) else {
                return ToolOutcome::err(format!("name_regex {pattern:?} is not a valid regex"));
            };
            let names: Vec<String> = ctx
                .shell_names()
                .into_iter()
                .filter(|name| regex.is_match(name))
                .collect();
            if names.is_empty() {
                return ToolOutcome::ok(format!("no live shell matches {pattern:?}"));
            }
            let jobs = ctx.detached.list(&ctx.session).await;
            let mut survivors = 0;
            let mut discarded = Vec::new();
            for name in &names {
                survivors += jobs
                    .iter()
                    .filter(|j| j.shell == *name && j.running)
                    .count();
                if ctx.forget(name) {
                    discarded.push(name.clone());
                }
            }
            let list = discarded.join(", ");
            // Detached jobs are not the shells', so they keep running; say so,
            // or the model would assume discarding the shells stopped them.
            if survivors == 0 {
                ToolOutcome::ok(format!("discarded shell(s) {list}"))
            } else {
                ToolOutcome::ok(format!(
                    "discarded shell(s) {list}; {survivors} detached job(s) from them are \
                     still running and were not stopped -- `kill_bash` stops them"
                ))
            }
        })
    }
}

// ---------------------------------------------------------------------------
// kill_bash
// ---------------------------------------------------------------------------

pub struct KillBashTool;

impl Tool for KillBashTool {
    fn name(&self) -> &'static str {
        "kill_bash"
    }

    fn description(&self) -> &'static str {
        "Stop a command that was started with `bash --detached`, or every \
         detached job of one shell if you do not name a job. Detached jobs are \
         the only ones with nothing to kill them implicitly: `cancel`, \
         `del_bash` and even restarting the server all leave them running, \
         which is the point -- this is how they end. Job numbers come from \
         `list_bash`."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Shell the job was started from. Only used when `job` \
                                     is omitted. Defaults to `default`."
                },
                "job": {
                    "type": "integer",
                    "description": "Job number from `list_bash`. Omit it to stop every \
                                     detached job of `name`."
                }
            },
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let name = shell_name(&args);
            let job = args.get("job").and_then(|v| v.as_u64());
            let stopped = ctx.detached.stop(&ctx.session, name, job).await;
            if stopped.total() == 0 {
                return ToolOutcome::ok(match job {
                    Some(n) => format!("no detached job {n} in this session"),
                    None => format!("shell {name} has no detached jobs"),
                });
            }
            ToolOutcome::ok(format!(
                "stopped {} detached job(s){}",
                stopped.stopped,
                if stopped.failed > 0 {
                    format!("; {} could not be stopped", stopped.failed)
                } else {
                    String::new()
                }
            ))
        })
    }
}

/// How a spill file is named: `session-shell-task-callid`.
///
/// One function rather than two `format!`s because the two paths that spill --
/// a foreground result, and the late output of a command that outlived its wait
/// -- must produce names that sort and read the same way. The `task`, when the
/// model supplied one, is what makes a dump identifiable later; without it the
/// name is still unique, just less useful.
fn spill_label(session: &str, shell: &str, task: Option<&str>, call_id: &str) -> String {
    match task {
        Some(task) => format!("{session}-{shell}-{task}-{call_id}"),
        None => format!("{session}-{shell}-{call_id}"),
    }
}

/// The shell a tool call is about, or `default`.
fn shell_name(args: &Value) -> &str {
    args.get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BASH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_exposes_the_spec_tools() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        assert_eq!(
            reg.names(),
            vec!["bash", "check_bash", "del_bash", "list_bash", "kill_bash"]
        );
        assert!(reg.get("bash").is_some());
        assert!(reg.get("nope").is_none());
    }

    /// A rejected call has to carry enough of the schema for the next attempt to
    /// be a correction rather than another guess -- and it has to be assembled
    /// from the schema, not from a list of names somebody remembered to update.
    #[test]
    fn a_call_missing_a_required_argument_is_answered_with_its_own_schema() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let bash = reg.get("bash").expect("bash is registered");

        // The argument abbreviated -- the shape of the mistake that used to be
        // spelled out the other way round, hundreds of times per session.
        let message = argument_error(bash, &json!({"cmd": "ls"})).expect("rejected");
        assert!(message.contains("`command` is required"), "{message}");
        assert!(message.contains("did you mean `command`"), "{message}");
        // The whole argument list, so the next attempt cannot be a guess.
        for arg in [
            "name: string",
            "command: string",
            "task: string",
            "timeout_ms: integer",
            "background: boolean",
            "detached: boolean",
        ] {
            assert!(message.contains(arg), "{arg} missing from: {message}");
        }

        // A call with what it needs is not rejected...
        assert!(argument_error(bash, &json!({"command": "ls"})).is_none());
        // ...and neither is one carrying a key nothing declares: it would have
        // run, and refusing it buys a retry for nothing.
        assert!(argument_error(bash, &json!({"command": "ls", "cwd": "/tmp"})).is_none());

        // Arguments that are not an object say what shape was expected.
        let message = argument_error(bash, &json!("ls")).expect("rejected");
        assert!(message.contains("must be a JSON object"), "{message}");
        assert!(message.contains("this was a string"), "{message}");
    }

    /// `task` names the dump a too-long output is spilled to. It must be
    /// offered to the model, must not become something the call can fail on,
    /// and must actually reach the file name.
    #[test]
    fn a_task_names_the_spill_file() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let bash = reg.get("bash").expect("bash is registered");
        let params = bash.parameters();
        assert!(params["properties"].get("task").is_some(), "{params}");
        // Still only `command`: a label drifting into a required argument would
        // turn a rejected call into a rejected *idea*, and the model would
        // stop describing what it is doing.
        assert_eq!(params["required"], serde_json::json!(["command"]), "{params}");
    }


    /// Both spill paths name files the same way, so a dump can be found again
    /// by the thing it was for.
    #[test]
    fn a_spill_file_is_named_after_what_the_call_was_for() {
        assert_eq!(
            spill_label("main", "default", Some("run the tests"), "call_1"),
            "main-default-run the tests-call_1"
        );
        // No task is not an error: the name stays unique, just less legible.
        assert_eq!(
            spill_label("main", "default", None, "call_1"),
            "main-default-call_1"
        );
        // The late path shares the rule, which is the point of having one.
        assert_eq!(
            spill_label("main", "default", Some("cargo build"), "late"),
            "main-default-cargo build-late"
        );
    }

    #[test]
    fn detached_jobs_have_an_explicit_way_to_end() {        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let kill = reg.get("kill_bash").expect("kill_bash is registered");
        // Omitting the job number means "every job of that shell", which is
        // the only shape that makes sense after a restart.
        assert!(kill.parameters()["properties"].get("job").is_some());
        // `job` is optional: omitting it means "every job of that shell", which
        // is the only shape that makes sense after a restart.
        assert!(
            kill.parameters()
                .get("required")
                .and_then(|r| r.as_array())
                .is_none_or(|r| !r.iter().any(|x| x == "job")),
            "job must be optional"
        );
        assert!(kill.description().contains("detached"));
    }

    #[test]
    fn bash_takes_name_command_and_the_two_detach_flags() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let defs = reg.definitions();
        let bash = &defs[0];
        assert_eq!(bash.name, "bash");
        assert_eq!(bash.parameters["required"][0], "command");
        let props = &bash.parameters["properties"];
        assert!(props.get("name").is_some(), "bash is nameable");
        assert!(props.get("command").is_some());
        assert!(props.get("background").is_some(), "bash can run in the background");
        assert!(props.get("detached").is_some(), "bash can run detached");
    }

    #[test]
    fn reading_tools_default_to_the_default_shell() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        for name in ["check_bash", "list_bash"] {
            let tool = reg.get(name).unwrap_or_else(|| panic!("{name} is registered"));
            let params = tool.parameters();
            if name == "check_bash" {
                assert!(params["properties"].get("name").is_some());
            }
        }
        assert_eq!(shell_name(&json!({})), DEFAULT_BASH);
        assert_eq!(shell_name(&json!({"name": "build"})), "build");
    }
}
