//! A persistent Python REPL, as one more tool beside the persistent shells.
//!
//! The interpreter is the environment's own: nothing is embedded. A small driver
//! on the far side reads one JSON request per line and answers with one JSON line,
//! so the protocol never depends on prompts, markers or shell quoting. State lives
//! in the driver's namespace between calls.
//!
//! See `docs/notes/server-tools-python.md`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::session::Machine;
use crate::tools::{Tool, ToolContext, ToolOutcome};

const DRIVER: &str = r#"
import ast, builtins, contextlib, io, json, sys, traceback
_ns = {"__name__": "__main__", "__builtins__": builtins}
def _run(request):
    code = request.get("code", "")
    out = io.StringIO()
    err = io.StringIO()
    ok = True
    saved_stdin = sys.stdin
    sys.stdin = io.StringIO("")
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                tree = ast.parse(code, "<python>", "exec")
                if len(tree.body) == 1 and isinstance(tree.body[0], ast.Expr):
                    value = eval(compile(ast.Expression(tree.body[0].value), "<python>", "eval"), _ns)
                    if value is not None:
                        print(repr(value))
                else:
                    exec(compile(tree, "<python>", "exec"), _ns)
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
        })
    }

    /// Runs one block and returns its captured standard output and error.
    pub async fn run(&mut self, code: &str, timeout: Duration) -> anyhow::Result<Reply> {
        let request = json!({ "code": code }).to_string();
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
    The namespace is one interpreter for the session: imports, variables, functions and \
    open files survive between calls, and only `restart` (or a timeout) replaces it. The \
    interpreter is the environment's own python, with no shell in between, so write the \
    program as ordinary Python and use `open()` / `Path.write_text()` for files; there is \
    no quoting to escape. Standard output and standard error are returned; an exception \
    is returned as a traceback and the namespace keeps the state it had reached. Use \
    `timeout_ms` for a long computation. `restart` ends the interpreter and starts a new \
    one for this call.";

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
                    "description": "Python source to execute in the persistent namespace. \
                                     A single expression is evaluated and its repr printed; \
                                     otherwise the block is executed."
                },
                "description": {
                    "type": "string",
                    "description": "What this call is for, in a few words -- read after the \
                                     tool name, never executed."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "How long to wait for this call. On timeout the \
                                     interpreter is stopped and the next call starts with a \
                                     fresh namespace."
                },
                "restart": {
                    "type": "boolean",
                    "description": "Stop the current interpreter and start a fresh one for \
                                     this call, discarding its namespace."
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
            let timeout = args["timeout_ms"]
                .as_u64()
                .map(Duration::from_millis)
                .unwrap_or(self.timeout);
            let restart = args["restart"].as_bool().unwrap_or(false);

            let mut guard = ctx.pythons.lock().await;
            if restart {
                if let Some(mut repl) = guard.take() {
                    repl.kill().await;
                }
            }
            if guard.is_none() {
                match PythonRepl::spawn(self.program.as_deref(), &self.args, &ctx.cwd) {
                    Ok(repl) => *guard = Some(repl),
                    Err(e) => return ToolOutcome::err(format!("python did not start: {e}")),
                }
            }
            let repl = guard.as_mut().expect("just spawned");
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
                    ToolOutcome::ok(cap(&text, self.max_output_chars))
                }
                Err(e) => {
                    if let Some(mut repl) = guard.take() {
                        repl.kill().await;
                    }
                    ToolOutcome::err(format!(
                        "{e}. The interpreter was stopped; the next `python` call starts with a \
                         fresh namespace."
                    ))
                }
            }
        })
    }
}

/// Head and tail of an over-long answer, with the cut named.
fn cap(text: &str, max: usize) -> String {
    if max == 0 || text.len() <= max {
        return text.to_string();
    }
    let half = max / 2;
    let mut head_end = half.min(text.len());
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(half);
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n\n[... {} chars cut from the middle ...]\n\n{}",
        &text[..head_end],
        text.len() - head_end - (text.len() - tail_start),
        &text[tail_start..]
    )
}
