//! The file tools: one path in, one answer out, on the machine the call names.
//!
//! # Invariant
//!
//! A file is read and written by this machine's own Python, or by the peer's, and
//! never by a program this server embeds: `remote` decides which side, and a name
//! that resolves to no machine is refused rather than run here. See
//! `docs/notes/server-tools-python.md`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::python::{DRIVER, PythonRepl, Reply, reply_text};
use crate::tools::{Cmd, ToolContext, ToolOutcome};

/// One file operation: a fresh driver, one request, one answer. File state is
/// the file, not a namespace.
async fn one_shot(
    program: Option<&Path>,
    args: &[String],
    cwd: &Path,
    request: Value,
    timeout: Duration,
) -> anyhow::Result<Reply> {
    let mut repl = PythonRepl::spawn(program, args, cwd, None, &[])?;
    let reply = repl.run_request(&request, timeout).await;
    repl.kill().await;
    reply
}

/// The remote a file call's `remote` names, when it names one.
///
/// # Invariant
///
/// `none` and an empty value are this machine, the same reading
/// `crate::session::remote_request` gives a `--remote` everywhere else.
fn stated_remote(args: &Value) -> Option<String> {
    let stated = args.get("remote").and_then(Value::as_str).map(str::trim)?;
    crate::session::remote_request(stated)
}

/// The machine a file call runs on: the session's remote when its `remote` names it.
///
/// # Invariant
///
/// A file tool's paths belong to the machine its `remote` names; a name that is not
/// the session's own is refused in `run`, so this is the machine the call ran on.
fn file_machine(args: &Value, ctx: &ToolContext) -> Machine {
    match stated_remote(args) {
        Some(name) if ctx.remote.as_deref() == Some(name.as_str()) => Machine::Remote,
        _ => Machine::Local,
    }
}

/// Whether a file call runs on the session's remote, and the refusal when its
/// `remote` names another machine.
///
/// # Invariant
///
/// A session works on one machine: its directory is on the remote it names, and a
/// second peer would have no directory to be in. See `docs/notes/server-remote.md`.
fn file_remote(args: &Value, ctx: &ToolContext, tool: &str) -> Result<bool, String> {
    let Some(stated) = stated_remote(args) else {
        return Ok(false);
    };
    if ctx.remote.as_deref() == Some(stated.as_str()) {
        return Ok(true);
    }
    Err(match &ctx.remote {
        Some(here) => format!(
            "`{tool}` ran nothing: this session's directory is on remote {here:?}, and its \
             files are there -- {stated:?} is another machine, which this session has no \
             directory on. A session works on one machine: `set_session --remote {stated}` \
             moves it there, and ends these shells first."
        ),
        None => format!(
            "`{tool}` ran nothing: this session names no remote, so it has no peer to work \
             on, and {stated:?} is not one it can reach from here."
        ),
    })
}

/// The endpoint of the session's remote, for a file call that named it.
fn remote_endpoint(ctx: &ToolContext) -> Option<crate::remote::Endpoint> {
    ctx.shells
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .endpoint(Machine::Remote)
}

/// One file operation on the session's remote, through an interpreter there.
///
/// # Invariant
///
/// The same driver as this machine's, started by name on the peer: a path this
/// machine holds is not evidence about one the peer has.
async fn one_shot_remote(
    endpoint: &crate::remote::Endpoint,
    cwd: &Path,
    request: Value,
    timeout: Duration,
) -> anyhow::Result<Reply> {
    let mut repl = crate::tools::python_remote::RemoteRepl::spawn(endpoint, cwd, DRIVER).await?;
    let reply = repl.run_request(&request, timeout).await;
    repl.kill().await;
    reply
}

/// The `remote` argument the file tools take: the session's remote, or this machine.
///
/// # Invariant
///
/// Offered only to a session that has a remote: `definitions` drops it otherwise, so
/// the argument is its own whole explanation and the descriptions below never mention
/// a second machine.
pub struct ReadFileTool {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Cmd for ReadFileTool {
    /// `--remote` is an argument of the call: a file command reads the machine the
    /// call names, wherever the session stands.
    fn machine(&self, args: &Value, ctx: &ToolContext) -> Option<crate::session::Machine> {
        Some(file_machine(args, ctx))
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(path) = args["path"].as_str() else {
                return ToolOutcome::err("read_file requires a string `path`");
            };
            let on_remote = match file_remote(&args, ctx, "read_file") {
                Ok(on_remote) => on_remote,
                Err(why) => return ToolOutcome::err(why),
            };
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let request = json!({
                "op": "read",
                "path": path,
                "offset": args["offset"].as_u64().unwrap_or(0),
                "limit": args["limit"].as_u64().unwrap_or(0),
            });
            // The wait the tunnel resolved for this call, not this tool's built-in one.
            let wait = crate::tools::tunnel::wait_of(&args).unwrap_or(self.timeout);
            let outcome = if on_remote {
                let Some(endpoint) = remote_endpoint(ctx) else {
                    return ToolOutcome::err(
                        "read_file ran nothing: this session names a remote, but it has no \
                         endpoint to reach",
                    );
                };
                one_shot_remote(&endpoint, &ctx.dir(Machine::Remote), request, wait).await
            } else {
                one_shot(
                    self.program.as_deref(),
                    &self.args,
                    &ctx.dir(Machine::Local),
                    request,
                    wait,
                )
                .await
            };
            let machine = if on_remote {
                Machine::Remote
            } else {
                Machine::Local
            };
            match outcome {
                Ok(reply) if reply.ok => {
                    let text = if reply.stdout.is_empty() {
                        "(empty file)".to_string()
                    } else {
                        reply.stdout
                    };
                    ToolOutcome::ok(ctx.cap(
                        machine,
                        "read_file",
                        description,
                        text,
                        self.max_output_chars,
                    ))
                }
                Ok(reply) => ToolOutcome::err(reply_text(&reply)),
                Err(e) => ToolOutcome::err(format!("read_file did not run: {e}")),
            }
        })
    }
}

pub struct WriteFileTool {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Cmd for WriteFileTool {
    /// `--remote` is an argument of the call: a file command reads the machine the
    /// call names, wherever the session stands.
    fn machine(&self, args: &Value, ctx: &ToolContext) -> Option<crate::session::Machine> {
        Some(file_machine(args, ctx))
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let (Some(path), Some(content)) = (args["path"].as_str(), args["content"].as_str())
            else {
                return ToolOutcome::err("write_file requires string `path` and `content`");
            };
            let on_remote = match file_remote(&args, ctx, "write_file") {
                Ok(on_remote) => on_remote,
                Err(why) => return ToolOutcome::err(why),
            };
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let request = json!({"op": "write", "path": path, "content": content});
            // The wait the tunnel resolved for this call, not this tool's built-in one.
            let wait = crate::tools::tunnel::wait_of(&args).unwrap_or(self.timeout);
            let outcome = if on_remote {
                let Some(endpoint) = remote_endpoint(ctx) else {
                    return ToolOutcome::err(
                        "write_file ran nothing: this session names a remote, but it has no \
                         endpoint to reach",
                    );
                };
                one_shot_remote(&endpoint, &ctx.dir(Machine::Remote), request, wait).await
            } else {
                one_shot(
                    self.program.as_deref(),
                    &self.args,
                    &ctx.dir(Machine::Local),
                    request,
                    wait,
                )
                .await
            };
            let machine = if on_remote {
                Machine::Remote
            } else {
                Machine::Local
            };
            match outcome {
                Ok(reply) if reply.ok => ToolOutcome::ok(ctx.cap(
                    machine,
                    "write_file",
                    description,
                    reply_text(&reply),
                    self.max_output_chars,
                )),
                Ok(reply) => ToolOutcome::err(reply_text(&reply)),
                Err(e) => ToolOutcome::err(format!("write_file did not run: {e}")),
            }
        })
    }
}

pub struct ReplaceTool {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Cmd for ReplaceTool {
    /// `--remote` is an argument of the call: a file command reads the machine the
    /// call names, wherever the session stands.
    fn machine(&self, args: &Value, ctx: &ToolContext) -> Option<crate::session::Machine> {
        Some(file_machine(args, ctx))
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let (Some(path), Some(old), Some(new)) = (
                args["path"].as_str(),
                args["old"].as_str(),
                args["new"].as_str(),
            ) else {
                return ToolOutcome::err("replace requires string `path`, `old` and `new`");
            };
            if old.is_empty() {
                return ToolOutcome::err("replace requires a non-empty `old`");
            }
            let on_remote = match file_remote(&args, ctx, "replace") {
                Ok(on_remote) => on_remote,
                Err(why) => return ToolOutcome::err(why),
            };
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let request = json!({
                "op": "replace",
                "path": path,
                "old": old,
                "new": new,
                "all": args["all"].as_bool().unwrap_or(false),
            });
            // The wait the tunnel resolved for this call, not this tool's built-in one.
            let wait = crate::tools::tunnel::wait_of(&args).unwrap_or(self.timeout);
            let outcome = if on_remote {
                let Some(endpoint) = remote_endpoint(ctx) else {
                    return ToolOutcome::err(
                        "replace ran nothing: this session names a remote, but it has no \
                         endpoint to reach",
                    );
                };
                one_shot_remote(&endpoint, &ctx.dir(Machine::Remote), request, wait).await
            } else {
                one_shot(
                    self.program.as_deref(),
                    &self.args,
                    &ctx.dir(Machine::Local),
                    request,
                    wait,
                )
                .await
            };
            let machine = if on_remote {
                Machine::Remote
            } else {
                Machine::Local
            };
            match outcome {
                Ok(reply) if reply.ok => ToolOutcome::ok(ctx.cap(
                    machine,
                    "replace",
                    description,
                    reply_text(&reply),
                    self.max_output_chars,
                )),
                Ok(reply) => ToolOutcome::err(reply_text(&reply)),
                Err(e) => ToolOutcome::err(format!("replace did not run: {e}")),
            }
        })
    }
}
