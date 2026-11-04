//! Persistent Python interpreters, named like the persistent shells.
//!
//! # Invariant
//!
//! The interpreter is the environment's own: nothing is embedded. A driver on the
//! far side reads one JSON request per line and answers with one JSON line, so
//! the protocol never depends on prompts, markers or shell quoting.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;

use crate::session::Machine;
use crate::tools::{Tool, ToolContext, ToolOutcome};

const DEFAULT_NAME: &str = "default";

pub(crate) const DRIVER: &str = r#"
import ast, builtins, contextlib, io, json, os, stat, sys, tempfile, traceback
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

/// One live interpreter, started in the session's directory.
pub struct PythonRepl {
    /// Behind its own lock, so stopping this interpreter never waits for the call
    /// that is running in it.
    child: Arc<AsyncMutex<Child>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    program: PathBuf,
    cwd: PathBuf,
}

impl PythonRepl {
    pub fn spawn(program: Option<&Path>, args: &[String], cwd: &Path) -> anyhow::Result<Self> {
        let program = match program {
            Some(path) => path.to_path_buf(),
            None => detect_python()?,
        };
        let mut command = Command::new(&program);
        command
            .args(args)
            .arg("-u")
            .arg("-c")
            .arg(DRIVER)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning {}: {e}", program.display()))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
        Ok(Self {
            child: Arc::new(AsyncMutex::new(child)),
            stdin,
            stdout,
            program,
            cwd: cwd.to_path_buf(),
        })
    }

    pub fn control(&self) -> crate::tools::interpreter::Control {
        crate::tools::interpreter::Control::Local(self.child.clone())
    }

    pub fn pid(&self) -> Option<u32> {
        let child = self.child.try_lock();
        child.ok().and_then(|child| child.id())
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Sends one structured request; the driver's op decides what it does.
    pub async fn run_request(
        &mut self,
        request: &Value,
        timeout: Duration,
    ) -> anyhow::Result<Reply> {
        self.send(request).await?;
        let mut line = String::new();
        let read = tokio::time::timeout(timeout, self.stdout.read_line(&mut line)).await;
        match read {
            Err(_) => anyhow::bail!(
                "the Python call did not finish within {}s",
                timeout.as_secs()
            ),
            Ok(Ok(0)) => anyhow::bail!("the Python interpreter exited"),
            Ok(Ok(_)) => {
                let reply: Reply = serde_json::from_str(line.trim())
                    .map_err(|e| anyhow::anyhow!("a malformed Python reply: {e}"))?;
                Ok(reply)
            }
            Ok(Err(e)) => anyhow::bail!("reading from the Python driver: {e}"),
        }
    }

    /// Writes one request line to the driver.
    async fn send(&mut self, request: &Value) -> anyhow::Result<()> {
        let request = request.to_string();
        self.stdin
            .write_all(request.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("writing to the Python driver: {e}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| anyhow::anyhow!("writing to the Python driver: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("writing to the Python driver: {e}"))
    }

    /// Runs one call with no clock: a background job is stopped, never timed out.
    pub async fn run_untimed(&mut self, request: &Value) -> anyhow::Result<Reply> {
        self.send(request).await?;
        let mut line = String::new();
        match self.stdout.read_line(&mut line).await {
            Ok(0) => anyhow::bail!("the Python interpreter exited"),
            Ok(_) => Ok(serde_json::from_str::<Reply>(line.trim())
                .map_err(|e| anyhow::anyhow!("a malformed Python reply: {e}"))?),
            Err(e) => anyhow::bail!("reading from the Python driver: {e}"),
        }
    }

    /// Ends the interpreter; the next call starts a fresh namespace.
    pub async fn kill(&mut self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

/// One interpreter, wherever it runs.
///
/// # Invariant
///
/// A name is one interpreter: the map holds one of these per name, so `python` and
/// `remote_python` cannot both answer to it. See `docs/notes/server-tools-python.md`.
pub enum Repl {
    Local(PythonRepl),
    Remote(crate::tools::python_remote::RemoteRepl),
}

impl Repl {
    pub fn machine(&self) -> Machine {
        match self {
            Repl::Local(_) => Machine::Local,
            Repl::Remote(repl) => repl.machine(),
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            Repl::Local(repl) => repl.pid(),
            Repl::Remote(repl) => repl.pid(),
        }
    }

    pub fn program(&self) -> String {
        match self {
            Repl::Local(repl) => repl.program().display().to_string(),
            Repl::Remote(repl) => repl.program(),
        }
    }

    pub fn cwd(&self) -> &Path {
        match self {
            Repl::Local(repl) => repl.cwd(),
            Repl::Remote(repl) => repl.cwd(),
        }
    }

    pub async fn run_request(
        &mut self,
        request: &Value,
        timeout: Duration,
    ) -> anyhow::Result<Reply> {
        match self {
            Repl::Local(repl) => repl.run_request(request, timeout).await,
            Repl::Remote(repl) => repl.run_request(request, timeout).await,
        }
    }

    /// Runs one call with no clock, for a background job.
    pub async fn run_untimed(&mut self, request: &Value) -> anyhow::Result<Reply> {
        match self {
            Repl::Local(repl) => repl.run_untimed(request).await,
            Repl::Remote(repl) => repl.run_untimed(request).await,
        }
    }

    /// How an interpreter is stopped without taking its lock.
    pub fn control(&self) -> crate::tools::interpreter::Control {
        match self {
            Repl::Local(repl) => repl.control(),
            Repl::Remote(repl) => repl.control(),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

impl Reply {
    /// The status a background notice reports: a block either finished or raised.
    pub fn code(&self) -> Option<i32> {
        Some(if self.ok { 0 } else { 1 })
    }
}

fn detect_python() -> anyhow::Result<PathBuf> {
    for candidate in ["python3", "python"] {
        if let Ok(path) = which::which(candidate) {
            return Ok(path);
        }
    }
    anyhow::bail!("no python3 or python on PATH; set GONGGA_PYTHON or [tools.python] program")
}

const DESCRIPTION: &str = "`bash` with Python instead of a shell, on this machine. \
    Everything `bash` says about `name`, `description`, `timeout_ms` and `background` \
    holds here unchanged; `code` takes the place of `command`, and `restart` stops this \
    name's interpreter so the call starts a fresh namespace. State survives between \
    calls to one name because it is one process, so write ordinary Python -- there is \
    no shell quoting to escape. Standard output and standard error come back; an \
    exception comes back as a traceback and the namespace keeps what it had reached.";

/// What `remote_python` says it is: the same interpreter, on the session's peer.
///
/// # Invariant
///
/// It names the machine a path belongs to, because the one thing a model gets wrong
/// here is writing to this machine's file while meaning the peer's.
const REMOTE_DESCRIPTION: &str = "`python` on the session's remote endpoint, exactly \
    as `remote_bash` is `bash`: same arguments, same name space, same results. Paths are \
    that machine's, so `open()` here reads and writes the peer's files -- and \
    `read_file`, `write_file` and `replace` reach the same machine when their `remote` \
    names it.";

impl PythonTool {
    /// Starts one interpreter on this tool's machine.
    ///
    /// # Invariant
    ///
    /// A peer's Python is started by name there, never here: the program this machine
    /// resolved is not evidence about the one that machine has.
    async fn start(&self, ctx: &ToolContext, cwd: &Path) -> anyhow::Result<Repl> {
        match self.machine {
            Machine::Local => Ok(Repl::Local(PythonRepl::spawn(
                self.program.as_deref(),
                &self.args,
                cwd,
            )?)),
            Machine::Remote => {
                let endpoint = ctx
                    .shells
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .endpoint(Machine::Remote);
                let Some(endpoint) = endpoint else {
                    anyhow::bail!(
                        "this session names no remote, so there is no second machine to run \
                         Python on; `python` is the interpreter here"
                    );
                };
                Ok(Repl::Remote(
                    crate::tools::python_remote::RemoteRepl::spawn(&endpoint, cwd, DRIVER).await?,
                ))
            }
        }
    }
}

pub struct PythonTool {
    /// Which machine this tool drives. One implementation, two names.
    pub machine: Machine,
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Tool for PythonTool {
    fn name(&self) -> &'static str {
        match self.machine {
            Machine::Local => "python",
            Machine::Remote => "remote_python",
        }
    }

    fn description(&self) -> &'static str {
        match self.machine {
            Machine::Local => DESCRIPTION,
            Machine::Remote => REMOTE_DESCRIPTION,
        }
    }

    fn machine(&self, _args: &Value, _ctx: &ToolContext) -> Machine {
        self.machine
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python source to execute in the named interpreter. \
                                     A single expression is evaluated and its repr printed; \
                                     otherwise the block is executed."
                },
                "name": {
                    "type": "string",
                    "description": "Which interpreter this call is about. Omitted means \
                                     `default`; a new name starts a fresh interpreter in the \
                                     session's working directory."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the \
                                     tool name, never executed."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "How long to wait for this call. On timeout the \
                                     interpreter is stopped and the next call to that name \
                                     starts with a fresh namespace."
                },
                "restart": {
                    "type": "boolean",
                    "description": "Stop this name's current interpreter and start a fresh \
                                     one for this call, discarding its namespace."
                },
                "background": {
                    "type": "boolean",
                    "description": "Start the call and return at once, as `bash`'s \
                                    `background` does: `check_python` collects what it \
                                    printed, and this session is told when it ends."
                }
            },
            "required": ["code"]
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(code) = args["code"].as_str() else {
                return ToolOutcome::err("python requires a string `code`");
            };
            if code.trim().is_empty() {
                return ToolOutcome::err("python received empty `code`");
            }
            let name = args["name"]
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(DEFAULT_NAME);
            let timeout = args["timeout_ms"]
                .as_u64()
                .map(Duration::from_millis)
                .unwrap_or(self.timeout);
            let restart = args["restart"].as_bool().unwrap_or(false);
            let background = args["background"].as_bool().unwrap_or(false);
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());

            if restart {
                let old = ctx.pythons.lock().await.remove(name);
                if let Some(old) = old {
                    old.kill().await;
                }
            }
            // A name is one interpreter: the other tool's is refused, not borrowed.
            let existing = ctx.pythons.lock().await.get(name).cloned();
            if let Some(handle) = existing {
                // Read without the lock: a busy interpreter holds it for the whole call.
                let owner = handle.facts().machine;
                if owner != self.machine {
                    return ToolOutcome::err(format!(
                        "`{name}` is already a Python on {}: a name is one interpreter, and \
                         `python` and `remote_python` share the names. Name this one \
                         otherwise, or `del_python name_regex: \"^{name}$\"` first.",
                        owner.named()
                    ));
                }
                // Nothing here may read the answer to a call it did not make.
                if handle.jobs() > 0 {
                    return ToolOutcome::err(format!(
                        "`{name}` is running a background call, and this interpreter answers \
                         one call at a time: a foreground call now would read that one's \
                         answer. Read it with `check_python` (or `wait_ms`), or use another \
                         name."
                    ));
                }
            }
            let handle = {
                let mut pythons = ctx.pythons.lock().await;
                match pythons.get(name) {
                    Some(handle) => handle.clone(),
                    None => {
                        let repl = match self.start(ctx, &ctx.cwd).await {
                            Ok(repl) => repl,
                            Err(e) => {
                                return ToolOutcome::err(format!(
                                    "{} {name:?} did not start: {e}",
                                    self.name()
                                ));
                            }
                        };
                        let control = repl.control();
                        let facts = crate::tools::interpreter::Facts {
                            machine: repl.machine(),
                            pid: repl.pid(),
                            program: repl.program(),
                            cwd: repl.cwd().to_path_buf(),
                        };
                        let handle =
                            crate::tools::interpreter::Interpreter::new(repl, control, facts);
                        pythons.insert(name.to_string(), handle.clone());
                        handle
                    }
                }
            };

            let request = json!({ "code": code });
            if background {
                let notify = crate::tools::python_notifier(ctx, self.name(), name);
                handle.spawn_background(request, Some(notify));
                return ToolOutcome::ok(format!(
                    "started a background {} call in `{name}`; run `check_python` to collect \
                     what it printed, and this session is told when it ends",
                    self.name()
                ));
            }
            let mut repl = handle.inner.lock().await;
            match repl.run_request(&request, timeout).await {
                Ok(reply) => {
                    let text = crate::tools::interpreter::reply_text(&reply);
                    let text =
                        ctx.cap(self.machine, name, description, text, self.max_output_chars);
                    ToolOutcome::ok(text)
                }
                Err(e) => {
                    handle.kill().await;
                    drop(repl);
                    let mut pythons = ctx.pythons.lock().await;
                    if pythons
                        .get(name)
                        .is_some_and(|current| Arc::ptr_eq(current, &handle))
                    {
                        pythons.remove(name);
                    }
                    ToolOutcome::err(format!(
                        "{e}. Interpreter {name:?} was stopped; the next `{}` call to that \
                         name starts with a fresh namespace.",
                        self.name()
                    ))
                }
            }
        })
    }
}

/// The collector for interpreters: `check_bash`'s shape, for `python`.
pub struct CheckPythonTool;

impl Tool for CheckPythonTool {
    fn name(&self) -> &'static str {
        "check_python"
    }

    fn description(&self) -> &'static str {
        "What `check_bash` is to a shell, for an interpreter: the output a finished \
         `python` or `remote_python` background call printed, which reading clears. A name \
         names one interpreter on whichever machine holds it."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Interpreter to read. Defaults to `default`."
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "If this interpreter has background calls running, wait \
                                    up to this many milliseconds for them to finish before \
                                    reading. A timeout returns what is there and leaves them \
                                    running. Defaults to 0."
                }
            },
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let name = args["name"]
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(DEFAULT_NAME);
            let Some(handle) = ctx.pythons.lock().await.get(name).cloned() else {
                return ToolOutcome::err(format!(
                    "no interpreter named {name}; `list_python` says which names exist"
                ));
            };
            let wait = args["wait_ms"]
                .as_u64()
                .map(Duration::from_millis)
                .unwrap_or(Duration::ZERO)
                .min(Duration::from_secs(600));
            if !wait.is_zero() {
                handle.wait_idle(wait, &ctx.cancel).await;
            }
            let text = handle.take_pending();
            let jobs = handle.jobs();
            let where_it_is = format!("{name} on {}", handle.facts().machine.label());
            if text.is_empty() {
                return ToolOutcome::ok(if jobs == 0 {
                    format!("{where_it_is}: no unread output")
                } else {
                    format!("{where_it_is}: {jobs} call(s) still running, no output yet")
                });
            }
            ToolOutcome::ok(format!(
                "{text}\n[{where_it_is}: {jobs} call(s) still running]"
            ))
        })
    }
}

pub struct DelPythonTool;

impl Tool for DelPythonTool {
    fn name(&self) -> &'static str {
        "del_python"
    }

    fn description(&self) -> &'static str {
        "What `del_bash` is to a shell: the interpreters of this session whose name \
         matches a regex are stopped, and naming one again starts fresh -- no variables, \
         imports or open files from before, and any call it was running is gone with it."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name_regex": {
                    "type": "string",
                    "description": "Pattern matched against interpreter names, unanchored: \
                                    `qa` also takes `qa2`. Defaults to `^default$`."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the tool name, never executed."
                }
            },
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let pattern = args
                .get("name_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("^default$");
            let Ok(regex) = regex::Regex::new(pattern) else {
                return ToolOutcome::err(format!("name_regex {pattern:?} is not a valid regex"));
            };
            let (names, handles) = {
                let mut pythons = ctx.pythons.lock().await;
                let names: Vec<String> = pythons
                    .keys()
                    .filter(|name| regex.is_match(name))
                    .cloned()
                    .collect();
                let handles: Vec<Arc<crate::tools::interpreter::Interpreter>> = names
                    .iter()
                    .filter_map(|name| pythons.remove(name))
                    .collect();
                (names, handles)
            };
            if names.is_empty() {
                return ToolOutcome::ok(format!("no python matches {pattern:?}"));
            }
            for handle in handles {
                // Stopped without its lock: a name may be busy with a background call.
                handle.kill().await;
            }
            ToolOutcome::ok(format!("discarded python(s) {}", names.join(", ")))
        })
    }
}

pub struct ListPythonTool;

impl Tool for ListPythonTool {
    fn name(&self) -> &'static str {
        "list_python"
    }

    fn description(&self) -> &'static str {
        "What `list_bash` is to a shell: each interpreter's name, the machine it runs \
         on, pid, program, directory, running calls and whether anything unread is \
         waiting. `name_regex` narrows it."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name_regex": {
                    "type": "string",
                    "description": "Only names matching this pattern, unanchored: \
                                    `qa` also takes `qa2`. Defaults to every python."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the tool name, never executed."
                }
            },
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let pattern = args.get("name_regex").and_then(|v| v.as_str());
            let matches = match pattern {
                None => None,
                Some(pattern) => match regex::Regex::new(pattern) {
                    Ok(regex) => Some(regex),
                    Err(_) => {
                        return ToolOutcome::err(format!(
                            "name_regex {pattern:?} is not a valid regex"
                        ));
                    }
                },
            };
            let handles: Vec<(String, Arc<crate::tools::interpreter::Interpreter>)> = {
                let pythons = ctx.pythons.lock().await;
                pythons
                    .iter()
                    .filter(|(name, _)| matches.as_ref().is_none_or(|re| re.is_match(name)))
                    .map(|(name, handle)| (name.clone(), handle.clone()))
                    .collect()
            };
            if handles.is_empty() {
                return ToolOutcome::ok(match pattern {
                    None => "this session has no python".to_string(),
                    Some(pattern) => format!("no python matches {pattern:?}"),
                });
            }
            let mut rows = Vec::new();
            for (name, handle) in handles {
                // Nothing here takes the interpreter's lock: it may be mid-call.
                let facts = handle.facts();
                rows.push(json!({
                    "name": name,
                    "machine": facts.machine.label(),
                    "pid": facts.pid,
                    "program": facts.program,
                    "cwd": facts.cwd.display().to_string(),
                    "jobs": handle.jobs(),
                    "unread": !handle.take_pending().is_empty(),
                }));
            }
            ToolOutcome::ok(
                serde_json::to_string_pretty(&json!({ "python": rows })).unwrap_or_default(),
            )
        })
    }
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
    let mut repl = PythonRepl::spawn(program, args, cwd)?;
    let reply = repl.run_request(&request, timeout).await;
    repl.kill().await;
    reply
}

fn reply_text(reply: &Reply) -> String {
    let mut text = reply.stdout.clone();
    if !reply.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&reply.stderr);
    }
    text
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
fn remote_arg() -> Value {
    json!({
        "type": "string",
        "description": "Which machine the file is on. Omit it, or name `none`, for this \
                         machine and its own Python. Name the session's remote -- the \
                         machine `set_session --remote` named -- to work on that machine's \
                         file instead. Any other name is refused."
    })
}

pub struct ReadFileTool {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 file through the Python tool's interpreter on this machine, or on \
         the session's remote when `remote` names it. `offset` and `limit` are lines, \
         zero-based; `limit = 0` reads to the end. The content is a tool result, so it \
         never passes through shell quoting. Use it instead of `cat` when the file's \
         bytes matter."
    }

    fn machine(&self, args: &Value, ctx: &ToolContext) -> Machine {
        file_machine(args, ctx)
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to read."},
                "remote": remote_arg(),
                "offset": {
                    "type": "integer",
                    "description": "First line, zero-based. Defaults to 0."
                },
                "limit": {
                    "type": "integer",
                    "description": "How many lines; 0 means to the end. Defaults to 0."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the tool name, never executed."
                }
            },
            "required": ["path"]
        })
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
            let outcome = if on_remote {
                let Some(endpoint) = remote_endpoint(ctx) else {
                    return ToolOutcome::err(
                        "read_file ran nothing: this session names a remote, but it has no \
                         endpoint to reach",
                    );
                };
                one_shot_remote(&endpoint, &ctx.dir(Machine::Remote), request, self.timeout).await
            } else {
                one_shot(
                    self.program.as_deref(),
                    &self.args,
                    &ctx.dir(Machine::Local),
                    request,
                    self.timeout,
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

impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Write a file through the Python tool's interpreter on this machine, or on the \
         session's remote when `remote` names it: `content` is a JSON field, so it is \
         written exactly as given, with no shell or Python quoting layer. Missing parent \
         directories are created and the replacement is atomic. Use this instead of a \
         heredoc or `write_text` inside a `python` program."
    }

    fn machine(&self, args: &Value, ctx: &ToolContext) -> Machine {
        file_machine(args, ctx)
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to write."},
                "remote": remote_arg(),
                "content": {
                    "type": "string",
                    "description": "The complete UTF-8 content; this is the file, not a program."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the tool name, never executed."
                }
            },
            "required": ["path", "content"]
        })
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
            let outcome = if on_remote {
                let Some(endpoint) = remote_endpoint(ctx) else {
                    return ToolOutcome::err(
                        "write_file ran nothing: this session names a remote, but it has no \
                         endpoint to reach",
                    );
                };
                one_shot_remote(&endpoint, &ctx.dir(Machine::Remote), request, self.timeout).await
            } else {
                one_shot(
                    self.program.as_deref(),
                    &self.args,
                    &ctx.dir(Machine::Local),
                    request,
                    self.timeout,
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

impl Tool for ReplaceTool {
    fn name(&self) -> &'static str {
        "replace"
    }

    fn description(&self) -> &'static str {
        "Replace one exact, literal piece of a UTF-8 file through the Python tool's \
         interpreter on this machine, or on the session's remote when `remote` names \
         it. Without `all`, `old` must occur exactly once; any other count is an error, \
         so a stale context cannot silently rewrite the wrong place. With `all`, every \
         occurrence is replaced. The write is atomic. `old` and `new` are JSON fields, \
         not shell or Python syntax."
    }

    fn machine(&self, args: &Value, ctx: &ToolContext) -> Machine {
        file_machine(args, ctx)
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to edit."},
                "remote": remote_arg(),
                "old": {"type": "string", "description": "Exact text to replace."},
                "new": {"type": "string", "description": "Replacement text."},
                "all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring exactly one. Defaults to false."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the tool name, never executed."
                }
            },
            "required": ["path", "old", "new"]
        })
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
            let outcome = if on_remote {
                let Some(endpoint) = remote_endpoint(ctx) else {
                    return ToolOutcome::err(
                        "replace ran nothing: this session names a remote, but it has no \
                         endpoint to reach",
                    );
                };
                one_shot_remote(&endpoint, &ctx.dir(Machine::Remote), request, self.timeout).await
            } else {
                one_shot(
                    self.program.as_deref(),
                    &self.args,
                    &ctx.dir(Machine::Local),
                    request,
                    self.timeout,
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
