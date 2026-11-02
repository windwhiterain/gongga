//! Persistent Python interpreters, named like the persistent shells.
//!
//! The interpreter is the environment's own: nothing is embedded. A small driver
//! on the far side reads one JSON request per line and answers with one JSON line,
//! so the protocol never depends on prompts, markers or shell quoting. State lives
//! in the driver's namespace between calls.
//!
//! See `docs/notes/server-tools-python.md`.

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

const DRIVER: &str = r#"
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
    child: Child,
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
            child,
            stdin,
            stdout,
            program,
            cwd: cwd.to_path_buf(),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Runs one block and returns its captured standard output and error.
    pub async fn run(&mut self, code: &str, timeout: Duration) -> anyhow::Result<Reply> {
        self.run_request(&json!({ "code": code }), timeout).await
    }

    /// Sends one structured request; the driver's op decides what it does.
    pub async fn run_request(
        &mut self,
        request: &Value,
        timeout: Duration,
    ) -> anyhow::Result<Reply> {
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
            .map_err(|e| anyhow::anyhow!("writing to the Python driver: {e}"))?;

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

    /// Ends the interpreter; the next call starts a fresh namespace.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
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

fn detect_python() -> anyhow::Result<PathBuf> {
    for candidate in ["python3", "python"] {
        if let Ok(path) = which::which(candidate) {
            return Ok(path);
        }
    }
    anyhow::bail!("no python3 or python on PATH; set GONGGA_PYTHON or [tools.python] program")
}

const DESCRIPTION: &str = "Run Python in a persistent interpreter on this machine. \
    Each `name` is one interpreter: imports, variables, functions and open files \
    survive between calls to that name, and only `restart`, `del_python`, or a \
    timeout replaces it. Omit `name` for `default`. The interpreter is the \
    environment's own python, with no shell in between, so write the program as \
    ordinary Python and use `open()` / `Path.write_text()` for files; there is no \
    quoting to escape. Standard output and standard error are returned; an exception \
    is returned as a traceback and the namespace keeps the state it had reached. Use \
    `timeout_ms` for a long computation, and `list_python` / `del_python` to see and \
    end interpreters the way `list_bash` / `del_bash` do.";

pub struct PythonTool {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Tool for PythonTool {
    fn name(&self) -> &'static str {
        "python"
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn machine(&self, _ctx: &ToolContext) -> Machine {
        Machine::Local
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
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let handle = {
                let mut pythons = ctx.pythons.lock().await;
                if restart {
                    if let Some(old) = pythons.remove(name) {
                        let mut repl = old.lock().await;
                        repl.kill().await;
                    }
                }
                match pythons.get(name) {
                    Some(handle) => handle.clone(),
                    None => {
                        let repl = match PythonRepl::spawn(
                            self.program.as_deref(),
                            &self.args,
                            &ctx.cwd,
                        ) {
                            Ok(repl) => repl,
                            Err(e) => {
                                return ToolOutcome::err(format!(
                                    "python {name:?} did not start: {e}"
                                ));
                            }
                        };
                        let handle = Arc::new(AsyncMutex::new(repl));
                        pythons.insert(name.to_string(), handle.clone());
                        handle
                    }
                }
            };

            let mut repl = handle.lock().await;
            match repl.run(code, timeout).await {
                Ok(reply) => {
                    let mut text = reply.stdout;
                    if !reply.stderr.is_empty() {
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&reply.stderr);
                    }
                    if !reply.ok && !text.contains("Traceback") {
                        text.push_str("\n(the block raised)");
                    }
                    let text = ctx.cap(
                        Machine::Local,
                        name,
                        description,
                        text,
                        self.max_output_chars,
                    );
                    ToolOutcome::ok(if text.is_empty() {
                        "(no output)".to_string()
                    } else {
                        text
                    })
                }
                Err(e) => {
                    repl.kill().await;
                    drop(repl);
                    let mut pythons = ctx.pythons.lock().await;
                    if pythons
                        .get(name)
                        .is_some_and(|current| Arc::ptr_eq(current, &handle))
                    {
                        pythons.remove(name);
                    }
                    ToolOutcome::err(format!(
                        "{e}. Interpreter {name:?} was stopped; the next `python` call to that \
                         name starts with a fresh namespace."
                    ))
                }
            }
        })
    }
}

pub struct DelPythonTool;

impl Tool for DelPythonTool {
    fn name(&self) -> &'static str {
        "del_python"
    }

    fn description(&self) -> &'static str {
        "Delete the Python interpreters of this session whose name matches a regex. \
         The process is stopped; naming it again starts a fresh interpreter in the \
         session's working directory, with no variables, imports or open files from \
         before. Use it when an interpreter is wedged or polluted, and to clean up \
         names a long session invented."
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
                let handles: Vec<Arc<AsyncMutex<PythonRepl>>> = names
                    .iter()
                    .filter_map(|name| pythons.remove(name))
                    .collect();
                (names, handles)
            };
            if names.is_empty() {
                return ToolOutcome::ok(format!("no python matches {pattern:?}"));
            }
            for handle in handles {
                handle.lock().await.kill().await;
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
        "List the Python interpreters of this session: their names, pids, the program \
         each was started from, and the directory each is standing in. `name_regex` \
         narrows the answer to the names that match, which is how one name's state is \
         asked about."
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
            let handles: Vec<(String, Arc<AsyncMutex<PythonRepl>>)> = {
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
                let repl = handle.lock().await;
                rows.push(json!({
                    "name": name,
                    "pid": repl.pid(),
                    "program": repl.program().display().to_string(),
                    "cwd": repl.cwd().display().to_string(),
                }));
            }
            ToolOutcome::ok(
                serde_json::to_string_pretty(&json!({ "python": rows })).unwrap_or_default(),
            )
        })
    }
}

/// One file operation: a fresh driver, one request, one answer. File state is the
/// file, not a namespace, so a process that outlives the call would buy nothing.
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
        "Read a UTF-8 file through the Python tool's interpreter on this machine. \
         `offset` and `limit` are lines, zero-based; `limit = 0` reads to the end. \
         The content is a tool result, so it never passes through shell quoting. \
         Use it instead of `cat` when the file's bytes matter."
    }

    fn machine(&self, _ctx: &ToolContext) -> Machine {
        Machine::Local
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to read."},
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
            match one_shot(
                self.program.as_deref(),
                &self.args,
                &ctx.cwd,
                request,
                self.timeout,
            )
            .await
            {
                Ok(reply) if reply.ok => {
                    let text = if reply.stdout.is_empty() {
                        "(empty file)".to_string()
                    } else {
                        reply.stdout
                    };
                    ToolOutcome::ok(ctx.cap(
                        Machine::Local,
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
        "Write a file through the Python tool's interpreter on this machine: \
         `content` is a JSON field, so it is written exactly as given, with no \
         shell or Python quoting layer. Missing parent directories are created and \
         the replacement is atomic. Use this instead of a heredoc or `write_text` \
         inside a `python` program."
    }

    fn machine(&self, _ctx: &ToolContext) -> Machine {
        Machine::Local
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to write."},
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
            let description = args
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let request = json!({"op": "write", "path": path, "content": content});
            match one_shot(
                self.program.as_deref(),
                &self.args,
                &ctx.cwd,
                request,
                self.timeout,
            )
            .await
            {
                Ok(reply) if reply.ok => ToolOutcome::ok(ctx.cap(
                    Machine::Local,
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
         interpreter on this machine. Without `all`, `old` must occur exactly once; \
         any other count is an error, so a stale context cannot silently rewrite the \
         wrong place. With `all`, every occurrence is replaced. The write is atomic. \
         `old` and `new` are JSON fields, not shell or Python syntax."
    }

    fn machine(&self, _ctx: &ToolContext) -> Machine {
        Machine::Local
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to edit."},
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
            match one_shot(
                self.program.as_deref(),
                &self.args,
                &ctx.cwd,
                request,
                self.timeout,
            )
            .await
            {
                Ok(reply) if reply.ok => ToolOutcome::ok(ctx.cap(
                    Machine::Local,
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
