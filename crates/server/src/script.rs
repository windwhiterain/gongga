//! Commands a Python script registers with the server.
//!
//! See `docs/notes/server-script.md`.
//!
//! # Invariant
//!
//! A script is data to the server: it is discovered by scanning the scripts
//! directory, its commands are published like any other, and a call is one
//! subprocess with one JSON request on stdin and one JSON reply on stdout. No
//! Rust change is needed to add a command, and no front-end knows its name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use protocol::{ArgSpec, Args, CommandSpec, CommandTarget, CompletionSource};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::commands::Outcome;

/// A broken manifest must not wedge startup: this is a hang guard, not a budget.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(10);

/// The scripts a server may call, and the commands they declared.
pub struct Scripts {
    dir: PathBuf,
    /// Resolved interpreter; `None` means none was found, and every script
    /// call says so instead of failing obscurely.
    program: Option<PathBuf>,
    /// Handed to the script, so it can drive the harness with the CLI.
    env: Vec<(String, String)>,
    state: Mutex<State>,
    /// Pids of running scripts, so `cancel` does not leave one behind.
    running: Mutex<HashMap<u32, String>>,
    /// Set when stopping, so a killed script's caller is told why.
    stopping: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct State {
    /// The directory as it was last read; `None` means never read.
    fingerprint: Option<Vec<Fingerprint>>,
    commands: Vec<CommandSpec>,
    /// Command name -> the script that declared it.
    routes: HashMap<String, PathBuf>,
    /// One line per script that could not be read, kept for the diagnosis.
    problems: Vec<String>,
}

/// What changed when: a script edited or added is re-read on the next greeting.
#[derive(PartialEq, Eq, Clone)]
struct Fingerprint {
    path: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl Scripts {
    pub fn new(dir: PathBuf, program: Option<PathBuf>, env: Vec<(String, String)>) -> Arc<Self> {
        Arc::new(Self {
            dir,
            program,
            env,
            state: Mutex::new(State::default()),
            running: Mutex::new(HashMap::new()),
            stopping: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Every command the scripts declare, re-reading only what changed.
    pub async fn commands(&self) -> Vec<CommandSpec> {
        let now = self.scan_fingerprint();
        let mut state = self.state.lock().await;
        if state.fingerprint.as_ref() == Some(&now) {
            return state.commands.clone();
        }
        let (commands, routes, problems) = self.discover().await;
        state.fingerprint = Some(now);
        state.commands = commands;
        state.routes = routes;
        state.problems = problems;
        state.commands.clone()
    }

    /// A one-line diagnosis per script that could not be read, if any.
    pub async fn problems(&self) -> Vec<String> {
        self.state.lock().await.problems.clone()
    }

    /// Runs the script that declared `command`, and answers with what it said.
    pub async fn run(&self, command: &str, args: &Args) -> Outcome {
        let Some(script) = self.state.lock().await.routes.get(command).cloned() else {
            // Not a known script command: say why, rather than "unknown".
            let problems = self.problems().await;
            let why = if problems.is_empty() {
                format!("no script registers {command:?}; look in {:?}", self.dir)
            } else {
                format!("no script registers {command:?}: {}", problems.join("; "))
            };
            return Outcome::err(why);
        };
        let Some(program) = &self.program else {
            return Outcome::err(
                "no Python interpreter found; set [scripts] program in config.toml",
            );
        };
        let request = json!({
            "op": "run",
            "command": command,
            "args": args,
            "home": self.env.iter().find(|(k, _)| k == "GONGGA_HOME").map(|(_, v)| v.clone()),
            "server": self.env.iter().find(|(k, _)| k == "GONGGA_SERVER").map(|(_, v)| v.clone()),
        });
        let child = match self.spawn(program, &script) {
            Ok(c) => c,
            Err(e) => {
                return Outcome::err(format!(
                    "could not start {} for {command:?}: {e}",
                    script.display()
                ));
            }
        };
        let pid = child.id();
        if let Some(pid) = pid {
            self.running.lock().await.insert(pid, command.to_string());
        }
        let out = self.exchange(child, &request).await;
        if let Some(pid) = pid {
            self.running.lock().await.remove(&pid);
        }
        let out = match out {
            Ok(o) => o,
            Err(why) => {
                if self.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    return Outcome::err(format!(
                        "{command}: the server stopped while this was running, so it was ended"
                    ));
                }
                return Outcome::err(format!("{command} ({}): {why}", script.display()));
            }
        };
        self.reply_outcome(out)
    }

    /// Stops every script still running. Called when the server stops.
    pub async fn stop_all(&self) -> usize {
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let running: Vec<(u32, String)> = self
            .running
            .lock()
            .await
            .iter()
            .map(|(pid, name)| (*pid, name.clone()))
            .collect();
        for (pid, name) in &running {
            tracing::warn!(pid, command = %name, "stopping a script left running by the stop");
            crate::tools::detached::kill_tree(*pid).await;
        }
        running.len()
    }

    /// The directory and interpreter this server was built with, for one log line.
    pub fn describe(&self) -> String {
        match &self.program {
            Some(p) => format!("{} with {}", self.dir.display(), p.display()),
            None => format!("{} (no Python found)", self.dir.display()),
        }
    }

    fn spawn(&self, program: &Path, script: &Path) -> std::io::Result<tokio::process::Child> {
        // The child runs in the scripts directory: an absolute path or it
        // resolves twice.
        let script = std::path::absolute(script).unwrap_or_else(|_| script.to_path_buf());
        let mut cmd = Command::new(program);
        cmd.arg(script)
            .current_dir(std::path::absolute(&self.dir).unwrap_or_else(|_| self.dir.clone()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.spawn()
    }

    /// One request in, one reply out; any other shape is a diagnosis.
    async fn exchange(
        &self,
        mut child: tokio::process::Child,
        request: &Value,
    ) -> Result<Value, String> {
        let mut stdin = child.stdin.take().ok_or("the script has no stdin")?;
        let body = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        stdin
            .write_all(&body)
            .await
            .map_err(|e| format!("writing the request: {e}"))?;
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("waiting for it: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            return Err(format!(
                "exited with {}: {}",
                output.status,
                if tail.is_empty() { "no stderr" } else { tail }
            ));
        }
        if line.is_empty() {
            return Err("printed nothing on stdout".into());
        }
        serde_json::from_str(&line)
            .map_err(|e| format!("stdout is not one JSON object ({e}): {}", head(&line)))
    }

    /// Turns a script's reply into the outcome the caller reads.
    fn reply_outcome(&self, reply: Value) -> Outcome {
        let ok = reply.get("ok").and_then(Value::as_bool).unwrap_or(true);
        let message = reply
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(if ok { "done" } else { "the script refused" })
            .to_string();
        if !ok {
            return Outcome::err(message);
        }
        match reply.get("data") {
            Some(data) if !data.is_null() => Outcome::ok_with(message, data.clone()),
            _ => Outcome::ok(message),
        }
    }

    fn scan_fingerprint(&self) -> Vec<Fingerprint> {
        scripts_in(&self.dir)
            .into_iter()
            .map(|path| {
                let meta = std::fs::metadata(&path).ok();
                Fingerprint {
                    len: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    modified: meta.and_then(|m| m.modified().ok()),
                    path,
                }
            })
            .collect()
    }

    /// Asks every script what it registers; a failure is kept, not fatal.
    async fn discover(&self) -> (Vec<CommandSpec>, HashMap<String, PathBuf>, Vec<String>) {
        let paths = scripts_in(&self.dir);
        let Some(program) = self.program.clone() else {
            if paths.is_empty() {
                return (Vec::new(), HashMap::new(), Vec::new());
            }
            return (
                Vec::new(),
                HashMap::new(),
                vec!["no Python interpreter found; set [scripts] program".to_string()],
            );
        };
        let mut commands: Vec<CommandSpec> = Vec::new();
        let mut routes = HashMap::new();
        let mut problems = Vec::new();
        for path in paths {
            match self.manifest(&program, &path).await {
                Ok(declared) => {
                    for c in declared {
                        if commands.iter().any(|known| known.name == c.name) {
                            problems.push(format!(
                                "{} declares {:?}, which is already registered",
                                path.display(),
                                c.name
                            ));
                            continue;
                        }
                        routes.insert(c.name.clone(), path.clone());
                        commands.push(c);
                    }
                }
                Err(why) => problems.push(format!("{}: {why}", path.display())),
            }
        }
        for p in &problems {
            tracing::warn!(problem = %p, "script not loaded");
        }
        (commands, routes, problems)
    }

    async fn manifest(&self, program: &Path, script: &Path) -> Result<Vec<CommandSpec>, String> {
        let child = self
            .spawn(program, script)
            .map_err(|e| format!("could not start: {e}"))?;
        // The future owns the child, so a timeout drops it and `kill_on_drop`
        // ends the script rather than leaving it to hang.
        let deadline = tokio::time::timeout(
            DISCOVERY_DEADLINE,
            self.exchange(child, &json!({"op": "manifest"})),
        )
        .await;
        let reply = match deadline {
            Ok(reply) => reply?,
            Err(_) => {
                return Err(format!(
                    "no manifest within {}s; is it waiting on stdin?",
                    DISCOVERY_DEADLINE.as_secs()
                ));
            }
        };
        parse_manifest(&reply, script)
    }
}

/// The `*.py` files in `dir`, in a stable order so the first claim on a name wins.
fn scripts_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "py").unwrap_or(false) && p.is_file())
        .collect();
    paths.sort();
    paths
}

fn parse_manifest(reply: &Value, script: &Path) -> Result<Vec<CommandSpec>, String> {
    let list = reply
        .get("commands")
        .and_then(Value::as_array)
        .ok_or("the manifest has no `commands` array")?;
    let mut out = Vec::new();
    for entry in list {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("a command in {} has no `name`", script.display()))?;
        let summary = entry
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("Registered by a script.")
            .to_string();
        let args = entry
            .get("args")
            .and_then(Value::as_array)
            .map(|args| args.iter().filter_map(parse_arg).collect())
            .unwrap_or_default();
        out.push(CommandSpec {
            name: name.to_string(),
            summary,
            args,
            target: CommandTarget::ServerScript,
        });
    }
    Ok(out)
}

fn parse_arg(raw: &Value) -> Option<ArgSpec> {
    let name = raw.get("name").and_then(Value::as_str)?;
    let completions = match raw.get("completions").and_then(Value::as_str) {
        Some("sessions") => CompletionSource::Sessions,
        Some("paths") => CompletionSource::Paths,
        _ => CompletionSource::None,
    };
    Some(ArgSpec {
        name: name.to_string(),
        summary: raw
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        required: raw
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        value_hint: raw
            .get("value_hint")
            .and_then(Value::as_str)
            .map(str::to_string),
        completions,
    })
}

fn head(text: &str) -> String {
    text.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script that declares one command and answers it, written to a temp dir.
    const WEATHER: &str = r#"
import json, sys
req = json.load(sys.stdin)
if req["op"] == "manifest":
    print(json.dumps({"commands": [{
        "name": "weather",
        "summary": "Ask a script about the weather.",
        "args": [
            {"name": "city", "summary": "Which city.", "required": True,
             "value_hint": "city"},
            {"name": "session", "summary": "Where to report.", "required": False,
             "completions": "sessions"}
        ]
    }]}))
else:
    city = req["args"].get("city")
    print(json.dumps({"ok": True, "message": "clear in " + city,
                      "data": {"city": city, "echo": req["args"]}}))
"#;

    fn program() -> Option<PathBuf> {
        crate::config::detect_python()
    }

    fn scripts_with(dir: &Path) -> Arc<Scripts> {
        Scripts::new(dir.to_path_buf(), program(), vec![])
    }

    /// A script's command is published with its own args, as a script command.
    #[tokio::test]
    async fn a_script_registers_a_command() {
        let Some(_) = program() else {
            return; // No interpreter on this machine: nothing to prove here.
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weather.py"), WEATHER).unwrap();
        let scripts = scripts_with(dir.path());

        let commands = scripts.commands().await;
        assert_eq!(commands.len(), 1, "{commands:?}");
        let spec = &commands[0];
        assert_eq!(spec.name, "weather");
        assert_eq!(spec.target, CommandTarget::ServerScript);
        assert_eq!(spec.args.len(), 2);
        assert!(
            spec.args[0].required,
            "the script said the city is required"
        );
        assert!(
            matches!(spec.args[1].completions, CompletionSource::Sessions),
            "a declared source survives: {:?}",
            spec.args[1].completions
        );
        assert!(scripts.problems().await.is_empty());
    }

    /// Calling it is one subprocess: the args go in as JSON, the reply comes
    /// back as the command's own message and data.
    #[tokio::test]
    async fn a_script_command_runs_and_answers_with_data() {
        let Some(_) = program() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weather.py"), WEATHER).unwrap();
        let scripts = scripts_with(dir.path());
        scripts.commands().await;

        let args = Args::from([("city".to_string(), "Oslo".to_string())]);
        let out = scripts.run("weather", &args).await;
        assert!(out.ok, "{}", out.message);
        assert_eq!(out.message, "clear in Oslo");
        let data = out.data.expect("the script sent data");
        assert_eq!(data["city"], "Oslo");
        assert_eq!(data["echo"]["city"], "Oslo");
    }

    /// A script that fails says why, and does not take the catalog with it.
    #[tokio::test]
    async fn a_broken_script_is_diagnosed_not_fatal() {
        let Some(_) = program() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("good.py"), WEATHER).unwrap();
        std::fs::write(dir.path().join("bad.py"), "import sys\nprint('not json')\n").unwrap();
        let scripts = scripts_with(dir.path());

        let commands = scripts.commands().await;
        assert_eq!(commands.len(), 1, "the good one still loads: {commands:?}");
        assert_eq!(commands[0].name, "weather");
        let problems = scripts.problems().await;
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("bad.py"), "{problems:?}");
        assert!(problems[0].contains("JSON"), "{problems:?}");
    }

    /// An unregistered command says what is wrong, not "unknown".
    #[tokio::test]
    async fn running_an_unregistered_command_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = scripts_with(dir.path());
        let out = scripts.run("weather", &Args::default()).await;
        assert!(!out.ok);
        assert!(
            out.message.contains("no script registers"),
            "{}",
            out.message
        );
    }

    /// A script edited in place is re-read on the next greeting; before that,
    /// the cached list is what the server publishes.
    #[tokio::test]
    async fn an_edited_script_is_picked_up_without_a_restart() {
        let Some(_) = program() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weather.py"), WEATHER).unwrap();
        let scripts = scripts_with(dir.path());
        assert_eq!(scripts.commands().await.len(), 1);

        std::fs::write(
            dir.path().join("forecast.py"),
            WEATHER.replace("weather", "forecast"),
        )
        .unwrap();
        let commands = scripts.commands().await;
        let mut names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["forecast", "weather"]);
    }
}
