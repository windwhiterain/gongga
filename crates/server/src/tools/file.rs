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

use crate::tools::{Cmd, ToolContext, ToolOutcome};

pub(crate) const DRIVER: &str = r#"
import ast, builtins, contextlib, io, json, os, stat, sys, tempfile, traceback
# `import gongga_skill` works here: the skill contract is installed beside the home,
# and this is the one place a session's Python learns where.
_skill_lib = os.environ.get("GONGGA_SKILL_LIB")
if _skill_lib and _skill_lib not in sys.path:
    sys.path.insert(0, _skill_lib)
_ns = {"__name__": "__main__", "__builtins__": builtins}

def _atomic_write(path, data):
    path = os.path.abspath(path)
    parent = os.path.dirname(path) or "."
    os.makedirs(parent, exist_ok=True)
    fd, temp = tempfile.mkstemp(prefix=".gongga-", dir=parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        if os.path.exists(path):
            os.chmod(temp, stat.S_IMODE(os.stat(path).st_mode))
        os.replace(temp, path)
    except BaseException:
        try:
            os.unlink(temp)
        except OSError:
            pass
        raise

def _read(request):
    path = request["path"]
    offset = int(request.get("offset", 0))
    limit = int(request.get("limit", 0))
    if offset < 0 or limit < 0:
        raise ValueError("offset and limit must not be negative")
    with open(path, "rb") as handle:
        text = handle.read().decode("utf-8")
    lines = text.splitlines(keepends=True)
    chunk = lines[offset:] if limit == 0 else lines[offset:offset + limit]
    sys.stdout.write("".join(chunk))

def _write(request):
    path = request["path"]
    data = request["content"].encode("utf-8")
    _atomic_write(path, data)
    print(f"wrote {len(data)} bytes to {path}")

def _replace(request):
    path = request["path"]
    old = request["old"]
    new = request["new"]
    every = bool(request.get("all", False))
    if not old:
        raise ValueError("old must not be empty")
    with open(path, "rb") as handle:
        text = handle.read().decode("utf-8")
    count = text.count(old)
    if every:
        if count == 0:
            raise ValueError("old does not occur in the file")
        updated = text.replace(old, new)
    else:
        if count != 1:
            raise ValueError(f"old occurs {count} times; pass all=true or make it unique")
        updated = text.replace(old, new, 1)
    _atomic_write(path, updated.encode("utf-8"))
    print(f"replaced {count} occurrence(s) in {path}")

def _run(request):
    op = request.get("op", "exec")
    out = io.StringIO()
    err = io.StringIO()
    ok = True
    saved_stdin = sys.stdin
    sys.stdin = io.StringIO("")
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                if op == "read":
                    _read(request)
                elif op == "write":
                    _write(request)
                elif op == "replace":
                    _replace(request)
                elif op == "exec":
                    code = request.get("code", "")
                    tree = ast.parse(code, "<python>", "exec")
                    if len(tree.body) == 1 and isinstance(tree.body[0], ast.Expr):
                        value = eval(compile(ast.Expression(tree.body[0].value), "<python>", "eval"), _ns)
                        if value is not None:
                            print(repr(value))
                    else:
                        exec(compile(tree, "<python>", "exec"), _ns)
                else:
                    raise ValueError(f"unknown op {op!r}")
            except BaseException:
                ok = False
                traceback.print_exc()
    finally:
        sys.stdin = saved_stdin
        sys.__stdout__.write(json.dumps({"ok": ok, "stdout": out.getvalue(), "stderr": err.getvalue()}) + "\n")
        sys.__stdout__.flush()

for line in sys.stdin:
    try:
        request = json.loads(line)
    except Exception:
        continue
    _run(request)
"#;

#[derive(Debug, serde::Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

pub(crate) fn detect() -> anyhow::Result<PathBuf> {
    for candidate in ["python3", "python"] {
        if let Ok(path) = which::which(candidate) {
            return Ok(path);
        }
    }
    anyhow::bail!("no python3 or python on PATH; set GONGGA_PYTHON or [tools.python] program")
}

pub(crate) fn reply_text(reply: &Reply) -> String {
    let mut text = reply.stdout.clone();
    if !reply.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&reply.stderr);
    }
    text
}

/// One file operation: a fresh driver, one request, one answer. File state is
/// the file, not a namespace.
async fn one_shot(
    program: Option<&Path>,
    args: &[String],
    cwd: &Path,
    request: Value,
    timeout: Duration,
) -> anyhow::Result<Reply> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let program = match program {
        Some(path) => path.to_path_buf(),
        None => detect()?,
    };
    let mut child = tokio::process::Command::new(&program)
        .args(args)
        .arg("-u")
        .arg("-c")
        .arg(DRIVER)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning {}: {e}", program.display()))?;
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let line = request.to_string();
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("writing to the Python driver: {e}"))?;
    stdin.flush().await.ok();
    let mut answer = String::new();
    let read = tokio::time::timeout(timeout, stdout.read_line(&mut answer)).await;
    let reply = match read {
        Err(_) => Err(anyhow::anyhow!(
            "the Python call did not finish within {}s",
            timeout.as_secs()
        )),
        Ok(Ok(0)) => Err(anyhow::anyhow!("the Python interpreter exited")),
        Ok(Ok(_)) => serde_json::from_str::<Reply>(answer.trim())
            .map_err(|e| anyhow::anyhow!("a malformed Python reply: {e}")),
        Ok(Err(e)) => Err(anyhow::anyhow!("reading from the Python driver: {e}")),
    };
    let _ = child.kill().await;
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
