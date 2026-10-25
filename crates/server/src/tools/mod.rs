//! Tools the model calls: persistent shells and the jobs they leave behind.
//!
//! See `docs/notes/server-tools-mod.md`.

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
pub mod harness;
#[cfg(windows)]
pub mod job;
#[cfg(test)]
pub(crate) mod testbed;

/// Everything a tool invocation needs, supplied per call by the agent loop.
pub struct ToolContext {
    pub session: String,
    pub call_id: String,
    /// Where capped tool output is spilled, so nothing is lost by capping.
    pub output_dir: PathBuf,
    /// The session's named shells.
    pub shells: Arc<StdMutex<ShellSet>>,
    /// Detached jobs, keyed by session; they outlive their shell, their session and the server.
    pub detached: DetachedRegistry,
    /// Streaming sink for `tool_answer` events.
    pub events: Option<mpsc::Sender<ToolAnswerEvent>>,
    pub cancel: CancellationToken,
    /// Where a command that outlived its deadline reports back: an inbox row, not a result.
    pub store: Store,
    /// The session's working directory, for tools that take a relative path.
    pub cwd: PathBuf,
    /// What this turn's model can take in; a picture tool asks before sending one.
    pub vision: crate::llm::Vision,
    /// Whether a message arrived for this session that its turn cannot read yet.
    pub inbox: crate::session::InboxSignal,
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

    /// Retires `handle` and puts a fresh shell under `name`; false means another shell took it.
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

    /// Caps one command's output, spilling the rest to a file named after where it came from.
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
        Self {
            output: output.into(),
            ok: true,
        }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            ok: false,
        }
    }
}

/// A tool the model can call; a boxed future instead of `async_trait`, one macro less.
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
            ],
        }
    }

    /// Adds the tool that drives the harness itself.
    ///
    /// # Invariant
    ///
    /// Registered separately because it needs the dispatch that owns this
    /// registry: the caller builds the dispatch first, then hands its weak
    /// reference back here. See `main`.
    pub fn with_harness(
        mut self,
        dispatch: std::sync::Weak<crate::commands::Dispatch>,
        max_output_chars: usize,
    ) -> Self {
        self.tools.push(Box::new(harness::HarnessTool::new(
            dispatch,
            max_output_chars,
        )));
        self
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
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

/// Why a call does not fit the declared schema, in the schema's own words.
///
/// See `docs/notes/server-tools-mod.md`.
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
        .map(
            |wrong| match declared.iter().find(|d| is_shorthand_of(d, wrong)) {
                Some(right) => format!("`{wrong}` (did you mean `{right}`?)"),
                None => format!("`{wrong}`"),
            },
        )
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

/// Leaves a command that outlived its call behind: fresh shell, old one reports later.
///
/// # Invariant
///
/// Callable only once the future borrowing the shell has been dropped, because the
/// task it spawns takes that shell's lock to finish reading it.
///
/// See `docs/notes/server-tools-mod.md`.
async fn leave_running(
    ctx: &ToolContext,
    name: &str,
    handle: &Arc<Mutex<BashShell>>,
    task: Option<&str>,
    cap: usize,
    waited: Duration,
    because: &str,
) -> ToolOutcome {
    let retired = ctx.replace_shell(name, handle);
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
    // Same naming rule, with `late` where the call id goes: that call has already returned.
    let label = spill_label(&session, name, task, "late");
    let owned = name.to_string();
    let secs = waited.as_secs();
    let handle = handle.clone();
    tokio::spawn(async move {
        // Nobody holds this shell now, so the lock is free: the call that waited has returned.
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
        let text = crate::tools::bash::cap_output(&text, cap, &output_dir, &label);
        let text = if text.is_empty() {
            "(no output)".to_string()
        } else {
            text
        };
        let status = match code {
            Some(0) => "finished successfully".to_string(),
            Some(c) => format!("finished with exit status {c}"),
            None => "finished, though the exit status could not be read".to_string(),
        };
        let message = format!(
            "The command you started in shell `{owned}`, which outlived its \
             {secs}s wait and was left running, has now {status}.\n\n{text}"
        );
        if let Err(e) = store.add_pending_task(&session, "inject", &json!({"message": message})) {
            tracing::warn!(session = %session, "could not queue a finished command: {e}");
        }
    });
    ToolOutcome::ok(format!(
        "still running after {secs}s: {because}, so the wait ended and it was handed to the \
         background. The command itself was not stopped -- it continues in the old shell \
         `{name}`, which is now detached from this session, and its output will be posted \
         to this session when it finishes. `{name}` is a fresh shell now, in the session's \
         working directory.{}",
        if retired {
            ""
        } else {
            " (Another shell had already taken that name, so the old one is only reachable \
              through its completion message.)"
        }
    ))
}

/// Is one of these two names spelled out inside the other, in order?
///
/// See `docs/notes/server-tools-mod.md`.
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

/// The turn was cancelled while a command was running; its own type, not a message.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A message arrived for the session while this command ran: its own type because the shell lives on.
#[derive(Debug)]
struct WaitingMessage;

impl std::fmt::Display for WaitingMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a message arrived while the command was running")
    }
}

impl std::error::Error for WaitingMessage {}

/// How often a long wait checks for somebody trying to say something.
///
/// See `docs/notes/server-tools-mod.md`.
const INBOX_POLL: Duration = Duration::from_millis(250);

// ---------------------------------- bash ----------------------------------

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
                    "description": "Whichever bash this call is about. Without `detached`: \
                                     which persistent shell to run in, default `default` -- \
                                     reusing a name is what carries `cd`, exports and shell \
                                     functions from one call to the next. With `detached`: \
                                     the name of the detached bash being created, required, \
                                     and unique in the session -- it is what `del_bash` uses \
                                     to end that process later."
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
                                     `del_bash` with its name stops it. Defaults to false."
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
            // `command`, not `cmd`: the schema bends to the model, and a missing one is caught first.
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
            // Only ever a label: a missing or odd one costs a less informative path, not a failed call.
            let task = args
                .get("task")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            // Both flags return at once; they differ in what happens to the output, not in blocking.
            let background = args
                .get("background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let detached = args
                .get("detached")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if background && detached {
                return ToolOutcome::err(
                    "pick one: `background` collects the output, `detached` does not",
                );
            }
            // `detached` names the process itself; `background` is a job of a shell.
            if detached {
                // Defaulting to `default` would collide with the persistent shell.
                let Some(name) = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return ToolOutcome::err(
                        "`bash` with `detached: true` needs `name`: it is the only handle on a \
                         process nothing owns, and the default name would collide with the \
                         persistent shell called `default`",
                    );
                };
                let cwd = ctx.cwd.clone();
                return match ctx.detached.start(&ctx.session, name, command, &cwd).await {
                    Ok(job) => ToolOutcome::ok(format!(
                        "started detached bash `{}` (pid {}), running in {}. It outlives the \
                         shell, the session, `cancel` and even a server restart, so it is how \
                         you replace the server you are running on. Delete it with \
                         `del_bash name_regex: \"^{}$\"`, which stops the process. Its output \
                         is not collected -- read it at: {}",
                        job.name,
                        job.pid,
                        ctx.cwd.display(),
                        job.name,
                        job.log
                    )),
                    Err(e) => ToolOutcome::err(format!("{e}")),
                };
            }
            if background {
                let shell = match ctx.shell(name) {
                    Ok(s) => s,
                    Err(e) => return ToolOutcome::err(format!("{e}")),
                };
                let mut shell = shell.lock().await;
                return match shell.run_background(command).await {
                    Ok(job) => ToolOutcome::ok(format!(
                        "started background job {job} in shell {name}; \
                         run `check_bash` to collect its output"
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

            // Relay output as it arrives: draining after the command would block a chatty shell.
            let (tx, mut rx) = mpsc::channel::<BashChunk>(256);
            let mut shell = handle.lock().await;
            // Scoped so the future borrowing `shell` drops before the guard does.
            let started = std::time::Instant::now();
            let outcome = {
                let execute = shell.run(command, timeout, Some(&tx));
                tokio::pin!(execute);
                let mut punctual = tokio::time::interval(INBOX_POLL);
                loop {
                    tokio::select! {
                        res = &mut execute => break res,
                        _ = ctx.cancel.cancelled() => break Err(anyhow::Error::new(Cancelled)),
                        // Somebody is talking, and one command is not worth their wait. See the notes.
                        _ = punctual.tick() => {
                            if ctx.inbox.arrived() {
                                tracing::info!(
                                    session = %ctx.session, shell = %name, elapsed = ?started.elapsed(),
                                    "handed a command to the background: a message is waiting"
                                );
                                break Err(anyhow::Error::new(WaitingMessage));
                            }
                        }
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
            // A cancelled command is stopped, not abandoned: leftovers would answer the next call.
            let cancelled = matches!(&outcome, Err(e) if e.is::<Cancelled>());
            if cancelled {
                shell.kill().await;
            }
            // Read while the guard is held: a shell that lost its stream must not get another command.
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
                // Either the wait ran out or somebody spoke: nothing was stopped either way.
                Ok(o) if o.still_running => {
                    leave_running(
                        ctx,
                        name,
                        &handle,
                        task,
                        self.max_output_chars,
                        timeout,
                        "its deadline passed",
                    )
                    .await
                }
                Err(e) if e.is::<WaitingMessage>() => {
                    let waited = started.elapsed();
                    leave_running(
                        ctx,
                        name,
                        &handle,
                        task,
                        self.max_output_chars,
                        waited,
                        "a message arrived for this session",
                    )
                    .await
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
                        // Replace it now, not on the next call, so no command reads the previous one's text.
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
                    // Capped before it is stored: the transcript is replayed on every later step.
                    let text = ctx.cap(name, task, text, self.max_output_chars);
                    let text = if text.is_empty() {
                        "(no output)".to_string()
                    } else {
                        text
                    };
                    match o.exit_code {
                        Some(0) => ToolOutcome::ok(text),
                        Some(code) => ToolOutcome::err(format!("{text}\n[exit status {code}]")),
                        None => ToolOutcome::err(format!("{text}\n[shell exited]")),
                    }
                }
                Err(e) if e.is::<Cancelled>() => {
                    // Same as lost output: a shell stopped mid-flight is never handed another command.
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
                    // The write failed, so stdin may hold half a line; `healthy` is what says so.
                    if !healthy {
                        ctx.replace_shell(name, &handle);
                    }
                    ToolOutcome::err(format!("{e}"))
                }
            }
        })
    }
}

// ------------------------------- check_bash --------------------------------

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
            ToolOutcome::ok(format!("{text}\n[{name}: {} job(s) still running]", jobs))
        })
    }
}

// -------------------------------- list_bash --------------------------------

pub struct ListBashTool;

impl Tool for ListBashTool {
    fn name(&self) -> &'static str {
        "list_bash"
    }

    fn description(&self) -> &'static str {
        "List the bash of this session: their names, working directories, how much \
         unread output they are holding, and how many background jobs are still \
         running. Also lists the session's detached bashes, which are not any shell's \
         and survive them all. `name_regex` narrows the answer to the names that \
         match, which is how one name's state is asked about."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name_regex": {
                    "type": "string",
                    "description": "Only names matching this pattern, unanchored: \
                                    `qa` also takes `qa2`. Defaults to every bash."
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
            // The same rule `del_bash` follows: one pattern, both verbs.
            let pattern = args.get("name_regex").and_then(|v| v.as_str());
            let matches = match pattern {
                None => None,
                Some(pattern) => match regex::Regex::new(pattern) {
                    Ok(re) => Some(re),
                    Err(_) => {
                        return ToolOutcome::err(format!(
                            "name_regex {pattern:?} is not a valid regex"
                        ));
                    }
                },
            };
            let keep = |name: &str| matches.as_ref().is_none_or(|re| re.is_match(name));

            let mut rows = Vec::new();
            for name in ctx.shell_names() {
                if !keep(&name) {
                    continue;
                }
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
            // Detached bashes are the session's, not a shell's, and share the pattern.
            let detached: Vec<Value> = ctx
                .detached
                .list(&ctx.session)
                .await
                .iter()
                .filter(|j| keep(&j.name))
                .map(|j| j.to_json())
                .collect();
            if rows.is_empty() && detached.is_empty() {
                return ToolOutcome::ok(match pattern {
                    None => "this session has no bash".to_string(),
                    Some(p) => format!("no bash matches {p:?}"),
                });
            }
            ToolOutcome::ok(
                serde_json::to_string_pretty(&json!({"bash": rows, "detached": detached}))
                    .unwrap_or_default(),
            )
        })
    }
}

// -------------------------------- del_bash ---------------------------------

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
            // Unanchored on purpose: `qa` is how a shell family is named -- `qa`, `qa2`, `qa5`.
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
            // One word for both kinds: nothing else ends a detached process.
            let stopped = ctx.detached.stop(&ctx.session, pattern).await;
            if names.is_empty() {
                if stopped.total() == 0 {
                    return ToolOutcome::ok(format!("no bash matches {pattern:?}"));
                }
                return ToolOutcome::ok(format!(
                    "deleted {} detached bash(es) matching {pattern:?}{}",
                    stopped.stopped,
                    if stopped.failed > 0 {
                        format!("; {} could not be stopped", stopped.failed)
                    } else {
                        String::new()
                    }
                ));
            }
            let mut discarded = Vec::new();
            for name in &names {
                if ctx.forget(name) {
                    discarded.push(name.clone());
                }
            }
            let list = discarded.join(", ");
            let kept = if stopped.stopped > 0 {
                format!("; also deleted {} detached bash(es)", stopped.stopped)
            } else {
                String::new()
            };
            ToolOutcome::ok(format!("discarded shell(s) {list}{kept}"))
        })
    }
}

/// How a spill file is named: `session-shell-task-callid`, the same for both paths.
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
    use crate::session::InboxSignal;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A context wired to a throwaway store and one real shell, so a call can end to end.
    fn context_for(program: &Path, root: &Path, inbox: InboxSignal) -> ToolContext {
        let store = Store::open(&root.join("g.db")).expect("a throwaway store");
        let detached = DetachedRegistry::new(
            store.clone(),
            Some(program.to_path_buf()),
            root.join("detached"),
            Vec::new(),
        );
        let mut set = ShellSet::new(program.to_path_buf(), root.to_path_buf(), Vec::new());
        set.get(DEFAULT_BASH).expect("the default shell starts");
        ToolContext {
            session: "main".to_string(),
            call_id: "call_1".to_string(),
            output_dir: root.join("outputs"),
            shells: Arc::new(StdMutex::new(set)),
            detached,
            events: None,
            cancel: CancellationToken::new(),
            store,
            cwd: root.to_path_buf(),
            vision: crate::llm::Vision {
                accepts_image: false,
                refusal: String::new(),
            },
            inbox,
        }
    }

    /// The symptom that started this: a command sitting between you and an answer.
    #[tokio::test]
    async fn a_message_arriving_mid_command_hands_it_to_the_background() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("a scratch directory");
        let epoch = Arc::new(AtomicU64::new(0));
        let mut ctx = context_for(&program, dir.path(), InboxSignal::around(epoch.clone()));
        let tool = BashTool {
            timeout: Duration::from_secs(120),
            max_output_chars: 0,
        };

        // Somebody speaks a moment into a command that would run for two minutes.
        let a_message = epoch.clone();
        let speaker = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            a_message.fetch_add(1, Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        let outcome = tool
            .run(
                json!({"command": "sleep 20", "task": "hold the line"}),
                &ctx,
            )
            .await;
        let elapsed = started.elapsed();
        let _ = speaker.await;

        assert!(
            elapsed < Duration::from_secs(10),
            "the wait did not end early: {elapsed:?}\n{}",
            outcome.output
        );
        assert!(
            outcome.output.contains("handed to the background"),
            "the model was not told what happened: {}",
            outcome.output
        );
        assert!(
            outcome.output.contains("a message arrived"),
            "the reason was not named: {}",
            outcome.output
        );

        // The next call takes a fresh signal, as a real turn does, and the name
        // still means a shell it can use.
        ctx.inbox = InboxSignal::around(epoch.clone());
        let after = tool
            .run(json!({"command": "echo back in business"}), &ctx)
            .await;
        assert!(
            after.output.contains("back in business"),
            "the session could not go on working: {}",
            after.output
        );
    }

    #[test]
    fn registry_exposes_the_spec_tools() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        assert_eq!(
            reg.names(),
            vec!["bash", "check_bash", "del_bash", "list_bash"]
        );
        // Added by the server, which owns the dispatch it calls through.
        assert!(reg.get("harness").is_none());
        assert!(reg.get("bash").is_some());
        assert!(reg.get("nope").is_none());
    }

    /// A rejected call carries enough schema that the next attempt is a correction.
    #[test]
    fn a_call_missing_a_required_argument_is_answered_with_its_own_schema() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let bash = reg.get("bash").expect("bash is registered");

        // The argument abbreviated: the mistake that used to be spelled out the other way round.
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

    /// `task` names the spill file: offered to the model, never something a call fails on.
    #[test]
    fn a_task_names_the_spill_file() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let bash = reg.get("bash").expect("bash is registered");
        let params = bash.parameters();
        assert!(params["properties"].get("task").is_some(), "{params}");
        // Still only `command`: a required label would turn a rejected call into a rejected idea.
        assert_eq!(
            params["required"],
            serde_json::json!(["command"]),
            "{params}"
        );
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

    /// Listing takes the same pattern deleting does, so one rule for both.
    ///
    /// # Invariant
    ///
    /// A pattern that deletes also lists: an operator who can name a bash can see
    /// what that name will hit before it hits it.
    #[test]
    fn list_bash_takes_the_same_pattern_as_del_bash() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let list = reg.get("list_bash").expect("list_bash is registered");
        let del = reg.get("del_bash").expect("del_bash is registered");
        for (this, that) in [(list, del), (del, list)] {
            let mine = this.parameters();
            let theirs = that.parameters();
            assert_eq!(
                mine["properties"].get("name_regex").is_some(),
                theirs["properties"].get("name_regex").is_some(),
                "one takes a pattern, the other does not"
            );
            assert_eq!(mine["properties"]["name_regex"]["type"], "string");
        }
    }

    /// A pattern that matches nothing says so rather than returning an empty shape.
    #[tokio::test]
    async fn list_bash_with_a_pattern_that_matches_nothing_says_so() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let list = reg.get("list_bash").expect("registered");
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = context_for(
            &program,
            dir.path(),
            InboxSignal::around(Default::default()),
        );
        let out = list
            .run(json!({"name_regex": "^nothing-here$"}), &ctx)
            .await;
        assert!(out.ok, "{}", out.output);
        assert!(out.output.contains("no bash matches"), "{}", out.output);
    }

    /// A bad regex is refused, not treated as "match everything".
    #[tokio::test]
    async fn list_bash_refuses_a_bad_pattern() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let list = reg.get("list_bash").expect("registered");
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = context_for(
            &program,
            dir.path(),
            InboxSignal::around(Default::default()),
        );
        let out = list.run(json!({"name_regex": "("}), &ctx).await;
        assert!(
            !out.ok,
            "a malformed regex must not silently list everything"
        );
        assert!(out.output.contains("not a valid regex"), "{}", out.output);
    }

    /// A detached bash without a name is refused, not given the shell's.
    ///
    /// # Invariant
    ///
    /// The default name is `default`, which is also the persistent shell's name,
    /// so silently defaulting would make `del_bash{^default$}` delete both -- a
    /// process and a shell with one word meant for one of them.
    #[tokio::test]
    async fn a_detached_bash_needs_its_own_name() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = context_for(
            &program,
            dir.path(),
            InboxSignal::around(Default::default()),
        );
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let bash = reg.get("bash").expect("registered");
        let out = bash
            .run(json!({"command": "sleep 1", "detached": true}), &ctx)
            .await;
        assert!(!out.ok, "a nameless detached bash must be refused");
        assert!(out.output.contains("needs `name`"), "{}", out.output);
    }

    /// One word for both kinds: `del_bash` deletes a shell or a detached process.
    #[test]
    fn del_bash_is_the_way_a_detached_bash_ends() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        assert!(reg.get("kill_job").is_none(), "one noun, not two");
        let del = reg.get("del_bash").expect("del_bash is registered");
        assert!(del.parameters()["properties"].get("name_regex").is_some());
        assert!(
            del.description().contains("detached"),
            "the description must say it also ends a detached process: {}",
            del.description()
        );
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
        assert!(
            props.get("background").is_some(),
            "bash can run in the background"
        );
        assert!(props.get("detached").is_some(), "bash can run detached");
    }

    #[test]
    fn reading_tools_default_to_the_default_shell() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        for name in ["check_bash", "list_bash"] {
            let tool = reg
                .get(name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            let params = tool.parameters();
            if name == "check_bash" {
                assert!(params["properties"].get("name").is_some());
            }
        }
        assert_eq!(shell_name(&json!({})), DEFAULT_BASH);
        assert_eq!(shell_name(&json!({"name": "build"})), "build");
    }
}
