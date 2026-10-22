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
use crate::tools::bash::{BashChunk, BashShell, BashStream};
use crate::tools::detached::DetachedRegistry;

pub mod bash;
pub mod detached;

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
    /// Keeps `shell_name` out of the spill file's name too: a shell called
    /// `restart server` would otherwise put a space in a path.
    fn cap(&self, label: &str, text: String, max: usize) -> String {
        crate::tools::bash::cap_output(
            &text,
            max,
            &self.output_dir,
            &format!("{}-{}-{}", self.session, label, self.call_id),
        )
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
            Some(tool) => tool.run(args, ctx).await,
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
         functions between calls; omit `name` to use `default`. Output is \
         streamed as it is produced. Use `cd` to move around instead of passing \
         absolute paths every time. Very long output is capped: you get the \
         beginning and the end, and the full text is written to a file named in \
         the result -- read that file with `sed -n` or `grep`, do not cat it. \
         With `background`, the command is started and the call returns \
         immediately; collect what it printed later with `check_bash`."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Which persistent shell to use. Defaults to `default`."
                },
                "cmd": {
                    "type": "string",
                    "description": "The command line to execute."
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
                    "description": "Override the default wall-clock budget for this command. \
                                     Ignored for background commands."
                }
            },
            "required": ["cmd"],
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(command) = args.get("cmd").and_then(|v| v.as_str()) else {
                return ToolOutcome::err("bash requires a string `cmd`");
            };
            if command.trim().is_empty() {
                return ToolOutcome::err("bash received an empty command");
            }
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_BASH);
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

            let shell = match ctx.shell(name) {
                Ok(s) => s,
                Err(e) => return ToolOutcome::err(format!("{e}")),
            };

            // Relay shell output to `tool_answer` events while the command runs.
            // Draining has to be concurrent with the run itself: if we awaited
            // the command first, a chatty command would fill the channel and
            // block the shell.
            let (tx, mut rx) = mpsc::channel::<BashChunk>(256);
            let mut shell = shell.lock().await;
            // Scoped so the future borrowing `shell` is dropped before the guard
            // is, which is what lets the guard go out of scope at all.
            let outcome = {
                let execute = shell.run(command, timeout, Some(&tx));
                tokio::pin!(execute);
                loop {
                    tokio::select! {
                        res = &mut execute => break res,
                        _ = ctx.cancel.cancelled() => break Err(anyhow::anyhow!("cancelled")),
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
            drop(shell);
            while let Ok(chunk) = rx.try_recv() {
                let stream = match chunk.stream {
                    BashStream::Stdout => ToolStream::Stdout,
                    BashStream::Stderr => ToolStream::Stderr,
                };
                ctx.emit(stream, chunk.text).await;
            }

            match outcome {
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
                    // Capped before it is stored: what goes into the transcript
                    // is replayed on every later step, so the full output of a
                    // chatty command would be paid for over and over.
                    let text = ctx.cap(name, text, self.max_output_chars);
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
                        None if o.timed_out => ToolOutcome::err(text),
                        None => ToolOutcome::err(format!("{text}\n[shell exited]")),
                    }
                }
                Err(e) => ToolOutcome::err(format!("{e}")),
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
            let text = ctx.cap(name, shell.take_pending(), self.max_output_chars);
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
        "Discard a persistent shell. Its working directory, exports and shell \
         functions are lost; naming it again in `bash` starts a fresh shell in \
         the session's directory. Background jobs go with it; detached jobs do \
         not, because they were never the shell's. Use this when a shell is \
         wedged or polluted."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Shell to discard. Defaults to `default`."
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
            let survivors = ctx
                .detached
                .list(&ctx.session)
                .await
                .iter()
                .filter(|j| j.shell == name && j.running)
                .count();
            let discarded = ctx.forget(name);
            if !discarded {
                return ToolOutcome::ok(format!("no live shell named {name}"));
            }
            // Detached jobs are not the shell's, so they keep running; say so,
            // or the model would assume discarding the shell stopped them.
            if survivors == 0 {
                ToolOutcome::ok(format!("discarded shell {name}"))
            } else {
                ToolOutcome::ok(format!(
                    "discarded shell {name}; {survivors} detached job(s) from it are still \
                     running and were not stopped -- `kill_bash` stops them"
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

    #[test]
    fn detached_jobs_have_an_explicit_way_to_end() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
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
    fn bash_takes_name_cmd_and_the_two_detach_flags() {
        let reg = ToolRegistry::with_builtins(Duration::from_secs(30), 8_000);
        let defs = reg.definitions();
        let bash = &defs[0];
        assert_eq!(bash.name, "bash");
        assert_eq!(bash.parameters["required"][0], "cmd");
        let props = &bash.parameters["properties"];
        assert!(props.get("name").is_some(), "bash is nameable");
        assert!(props.get("cmd").is_some());
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
