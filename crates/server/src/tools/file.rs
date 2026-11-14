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

# One request, one answer, then the end: a file call is one shot, and the program
# ending by itself is what a caller on either machine waits for.
for line in sys.stdin:
    if not line.strip():
        continue
    try:
        request = json.loads(line)
    except Exception:
        continue
    _run(request)
    break
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
///
/// # Invariant
///
/// The request is answered where the call's path lives: the driver is started by the
/// space, so this machine and a peer are one code path. The answer is the last line the
/// driver wrote on its own standard output, because everything before it is the
/// program's own noise, and a call whose interpreter never started is an error naming
/// that program -- never a request sent a second time.
async fn one_shot(
    space: &crate::space::Space,
    program: &str,
    args: &[String],
    cwd: &Path,
    request: Value,
    timeout: Duration,
) -> anyhow::Result<Reply> {
    let mut line = request.to_string();
    line.push('\n');
    let run = match tokio::time::timeout(
        timeout,
        space.run(cwd, program, args, Some(line.into_bytes())),
    )
    .await
    {
        Err(_) => anyhow::bail!(
            "the Python call did not finish within {}s",
            timeout.as_secs()
        ),
        Ok(Err(e)) => anyhow::bail!("{e}"),
        Ok(Ok(run)) => run,
    };
    let printed = run.stdout_text();
    let answer = printed.lines().rev().find(|line| !line.trim().is_empty());
    let said = match run.stderr.is_empty() {
        true => String::new(),
        false => format!(
            "; the interpreter said: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        ),
    };
    match answer.map(|line| serde_json::from_str::<Reply>(line.trim())) {
        Some(Ok(reply)) => Ok(reply),
        Some(Err(e)) => anyhow::bail!("a malformed Python reply: {e}{said}"),
        None => anyhow::bail!(
            "the Python driver printed no answer and exited with {}{said}",
            match run.exit {
                Some(code) => format!("status {code}"),
                None => "no status".to_string(),
            }
        ),
    }
}

/// One request, answered by the interpreter on the machine the call's paths are on.
async fn answer(
    ctx: &ToolContext,
    configured: Option<&Path>,
    args: &[String],
    machine: Machine,
    request: Value,
    timeout: Duration,
) -> anyhow::Result<Reply> {
    let space = ctx.space();
    let cwd = ctx.dir(machine);
    let program = interpreter(&space, configured, &cwd).await?;
    one_shot(&space, &program, args, &cwd, request, timeout).await
}

/// The interpreter a file request is answered by, on the machine the paths are on.
///
/// # Invariant
///
/// A configured program is this machine's: it is a path the operator wrote here, and a
/// path this machine holds is not evidence about a peer's `PATH`. A peer's candidate is
/// believed only after it answers, because a name that resolves to the Windows Store's
/// stub exits having run nothing at all.
async fn interpreter(
    space: &crate::space::Space,
    configured: Option<&Path>,
    cwd: &Path,
) -> anyhow::Result<String> {
    if space.is_local() {
        if let Some(path) = configured {
            return Ok(path.to_string_lossy().into_owned());
        }
        return Ok(detect()?.to_string_lossy().into_owned());
    }
    let mut tried = Vec::new();
    for candidate in ["python3", "python"] {
        let probe = space
            .run(
                cwd,
                candidate,
                &["-c".to_string(), "print(1)".to_string()],
                None,
            )
            .await;
        match probe {
            Ok(run) if run.exit == Some(0) && run.stdout_text().trim() == "1" => {
                return Ok(candidate.to_string());
            }
            Ok(run) => tried.push(format!(
                "{candidate}: it answered {:?}",
                run.stdout_text().trim()
            )),
            Err(e) => tried.push(format!("{candidate}: {e}")),
        }
    }
    anyhow::bail!(
        "{} has no Python to start: {}. Install one on that machine, or run this on the machine that has it",
        space.describe(),
        tried.join("; ")
    )
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

/// The machine a file call's paths belong to.
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

/// The refusal when a file call names a machine the session has no directory on.
///
/// # Invariant
///
/// A session works on one machine: its directory is on the remote it names, and a
/// second peer would have no directory to be in. See `docs/notes/server-remote.md`.
fn file_remote(args: &Value, ctx: &ToolContext, tool: &str) -> Result<(), String> {
    let Some(stated) = stated_remote(args) else {
        return Ok(());
    };
    if ctx.remote.as_deref() == Some(stated.as_str()) {
        return Ok(());
    }
    Err(match &ctx.remote {
        Some(here) => format!(
            "`{tool}` ran nothing: this session's directory is on remote {here:?}, and its files are there -- {stated:?} is another machine, which this session has no directory on. A session works on one machine: `set_session --remote {stated}` moves it there, and ends these shells first."
        ),
        None => format!(
            "`{tool}` ran nothing: this session names no remote, so it has no peer to work on, and {stated:?} is not one it can reach from here."
        ),
    })
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
            if let Err(why) = file_remote(&args, ctx, "read_file") {
                return ToolOutcome::err(why);
            }
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
            let machine = file_machine(&args, ctx);
            let outcome = answer(
                ctx,
                self.program.as_deref(),
                &self.args,
                machine,
                request,
                self.timeout,
            )
            .await;
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
            if let Err(why) = file_remote(&args, ctx, "write_file") {
                return ToolOutcome::err(why);
            }
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let request = json!({"op": "write", "path": path, "content": content});
            let machine = file_machine(&args, ctx);
            let outcome = answer(
                ctx,
                self.program.as_deref(),
                &self.args,
                machine,
                request,
                self.timeout,
            )
            .await;
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
            if let Err(why) = file_remote(&args, ctx, "replace") {
                return ToolOutcome::err(why);
            }
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
            let machine = file_machine(&args, ctx);
            let outcome = answer(
                ctx,
                self.program.as_deref(),
                &self.args,
                machine,
                request,
                self.timeout,
            )
            .await;
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
