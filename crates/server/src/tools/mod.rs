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

use crate::remote::shell_set::Shell;
use crate::session::DEFAULT_BASH;
use crate::session::{Machine, Shells};
use crate::store::Store;
use crate::tools::bash::{BashChunk, BashStream};
use crate::tools::detached::DetachedRegistry;

pub mod bash;
pub mod detached;
pub mod index_skill;
pub mod interpreter;
#[cfg(windows)]
pub mod job;
pub mod python;
pub mod python_remote;

/// Everything a tool invocation needs, supplied per call by the agent loop.
pub struct ToolContext {
    pub session: String,
    pub call_id: String,
    /// Where capped tool output is spilled, so nothing is lost by capping.
    pub output_dir: PathBuf,
    /// The session's named shells, on this machine and -- when it names one --
    /// on its remote. See `crate::session::Shells`.
    pub shells: Arc<StdMutex<Shells>>,
    /// The session's interpreters, named like shells: one name, one interpreter.
    pub pythons:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, Arc<interpreter::Interpreter>>>>,
    /// The connection this session's references are read through, when its
    /// directory is on a remote.
    pub reader: Option<Arc<crate::remote::Reader>>,
    /// The machine this session's directory belongs to.
    ///
    /// # Invariant
    ///
    /// A tool that runs on a named machine carries that machine itself and ignores
    /// this field; this is where the *session* stands, which is what reads a path a
    /// message names. Nothing infers a machine from how a path is spelled.
    pub machine: Machine,
    /// The peer that machine is, when it is one.
    ///
    /// # Invariant
    ///
    /// `None` is this machine. A tool that takes a target resolves it against this
    /// and nothing else: a session's workspace is on one peer, so no other name can
    /// be where it stands.
    pub remote: Option<String>,
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
    /// The model this turn runs on, for the token ratio.
    pub model: String,
    /// What a plan for that model is priced at, from the roster and this machine.
    pub rates: crate::prefix_cache::Rates,
    /// Set when shells end because the server is going: nothing that stops then finished.
    pub closing: Arc<std::sync::atomic::AtomicBool>,
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
                    // Only the chunk that ends the call knows how it went.
                    head: None,
                })
                .await;
        }
    }

    /// The handle for `name` on `machine`, spawning the shell if this is its first use.
    fn shell(&self, machine: Machine, name: &str) -> anyhow::Result<Arc<Mutex<Shell>>> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(machine, name)
    }

    /// The handle without spawning, for tools that must not create one
    /// (`check_bash`, `list_bash`).
    fn peek(&self, machine: Machine, name: &str) -> Option<Arc<Mutex<Shell>>> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(machine, name)
    }

    /// Where `name` lives, whichever machine holds it.
    fn find(&self, name: &str) -> Option<(Machine, Arc<Mutex<Shell>>)> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .find(name)
    }

    /// Every shell that exists right now, as `(machine, name)`.
    fn shell_names(&self) -> Vec<(Machine, String)> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .names()
    }

    /// Takes every shell filed under `name`, on any machine.
    fn discard(&self, name: &str) -> Vec<crate::remote::shell_set::Discarded> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .discard(name)
    }

    /// Retires `handle` and puts a fresh shell under `name`; false means another shell took it.
    fn replace_shell(&self, machine: Machine, name: &str, handle: &Arc<Mutex<Shell>>) -> bool {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(machine, name, handle)
    }

    /// Whether the session's shells on `machine` are somewhere else.
    pub(crate) fn on_endpoint(&self, machine: Machine) -> bool {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_remote(machine)
    }

    /// The connection this session's references are read through, if any.
    pub(crate) fn reader(&self) -> Option<Arc<crate::remote::Reader>> {
        self.reader.clone()
    }

    /// Where `machine`'s shells stand, read without their locks.
    pub(crate) fn dir(&self, machine: Machine) -> PathBuf {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reported_cwd(machine)
    }

    /// Caps one command's output, spilling the rest to a file named after it.
    pub(crate) fn cap(
        &self,
        machine: Machine,
        shell: &str,
        description: Option<&str>,
        text: String,
        max: usize,
    ) -> String {
        let label = spill_label(&self.session, shell, description, &self.call_id);
        // A cut on an endpoint session is not spilled here: a path on this machine
        // is not a path the reader can open.
        let dir = (!self.on_endpoint(machine)).then_some(self.output_dir.as_path());
        crate::tools::bash::cap_output(crate::prompts::shared(), &text, max, dir, &label)
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

    /// Which machine this call's paths belong to.
    ///
    /// # Invariant
    ///
    /// A shell tool answers for the machine it drives; every other tool answers for
    /// where the session stands, unless its own arguments name a machine. That is
    /// the whole of "whose file is this".
    fn machine(&self, _args: &Value, ctx: &ToolContext) -> Machine {
        ctx.machine
    }

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
                    machine: Machine::Local,
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Box::new(BashTool {
                    machine: Machine::Remote,
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Box::new(CheckBashTool { max_output_chars }),
                Box::new(WaitTool {
                    timeout: bash_timeout,
                }),
                Box::new(DelBashTool),
                Box::new(ListBashTool),
                Box::new(index_skill::IndexSkillTool),
                Box::new(python::CheckPythonTool),
                Box::new(python::DelPythonTool),
                Box::new(python::ListPythonTool),
            ],
        }
    }

    /// Adds the persistent Python tool with the configuration the server resolved.
    pub fn with_python(mut self, python: crate::config::PythonConfig) -> Self {
        let files = python.clone();
        // One implementation, two names: here, and on the session's peer.
        for machine in [Machine::Local, Machine::Remote] {
            self.tools.push(Box::new(python::PythonTool {
                machine,
                program: python.program.clone(),
                args: python.args.clone(),
                timeout: python.timeout,
                max_output_chars: python.max_output_chars,
            }));
        }
        self.tools.push(Box::new(python::ReadFileTool {
            program: files.program.clone(),
            args: files.args.clone(),
            timeout: files.timeout,
            max_output_chars: files.max_output_chars,
        }));
        self.tools.push(Box::new(python::WriteFileTool {
            program: files.program.clone(),
            args: files.args.clone(),
            timeout: files.timeout,
            max_output_chars: files.max_output_chars,
        }));
        self.tools.push(Box::new(python::ReplaceTool {
            program: files.program,
            args: files.args,
            timeout: files.timeout,
            max_output_chars: files.max_output_chars,
        }));
        self
    }

    /// Every tool, in registration order: the catalog generator's one source.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Tool> {
        self.tools.iter().map(|tool| tool.as_ref())
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// Which machine a named tool's paths belong to.
    pub fn machine_of(&self, name: &str, args: &Value, ctx: &ToolContext) -> Machine {
        match self.get(name) {
            Some(tool) => tool.machine(args, ctx),
            None => ctx.machine,
        }
    }

    /// Tool schemas in the shape the Responses API expects.
    ///
    /// # Invariant
    ///
    /// `remote_bash` is offered only to a session that has a remote: a tool the model
    /// can call but that can never answer is a wrong turn nobody can undo.
    pub fn definitions(&self, has_remote: bool) -> Vec<crate::llm::ToolDefinition> {
        self.tools
            .iter()
            .filter(|t| has_remote || t.name() != REMOTE_BASH)
            .map(|t| crate::llm::ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: general_parameters(t.parameters()),
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

/// The adapter's general parameters: every tool may carry them, none
/// declares them by hand.
///
/// # Invariant
///
/// Injected into the model-facing schema in `definitions` (a declared
/// property wins), read back at the call layer; they never reach the
/// command surface, which reads `Tool::parameters` raw.
/// `Tool::parameters` raw.
pub const GENERAL_ARGS: &[&str] = &["description", "max_output"];

/// SPEC's adapter-level parameters added to every tool the model is offered.
///
/// # Invariant
///
/// Injected here, in the model-facing schema only, so a provider's validator
/// accepts what the call layer reads. A property a tool already declares wins:
/// these are the defaults, not an override.
fn general_parameters(mut schema: Value) -> Value {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return schema;
    };
    properties
        .entry("description".to_string())
        .or_insert_with(|| {
            json!({
                "type": "string",
                "description": "What this call is for, in a few words: the log carries it \
                                as the call's title.",
            })
        });
    properties
        .entry("max_output".to_string())
        .or_insert_with(|| {
            json!({
                "type": "integer",
                "description": "In chars: when the output exceeds this, it is saved to a \
                                temp file and the call answers with the file's metadata \
                                {path, chars, line_count}.",
            })
        });
    schema
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
        .filter(|k| !declared.contains(&k.as_str()) && !GENERAL_ARGS.contains(&k.as_str()))
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
            let kind = kind_of(spec);
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

/// What a property accepts, in the words of its own schema.
///
/// # Invariant
///
/// A property that offers alternatives (`anyOf`) says so, because "any" would
/// hide the choice the caller still has to make.
pub(crate) fn kind_of(spec: &Value) -> String {
    if let Some(kinds) = spec.get("anyOf").and_then(Value::as_array) {
        let kinds: Vec<&str> = kinds
            .iter()
            .filter_map(|kind| kind["type"].as_str())
            .collect();
        if !kinds.is_empty() {
            return kinds.join(" or ");
        }
    }
    spec["type"].as_str().unwrap_or("any").to_string()
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
/// Where a finished background job reports: an inbox row, not a result.
///
/// # Invariant
///
/// The row is an `inject`, so a busy session reads it at its next step boundary and
/// an idle one is woken for it (`commands::nudge_inboxes`). See
/// `docs/notes/server-tools-mod.md`.
fn background_notifier(ctx: &ToolContext, shell: &str) -> bash::BackgroundNotifier {
    let what = format!("in shell `{shell}`");
    let where_to_read = format!("`check_bash` with name `{shell}` collects it");
    notice_background_end(ctx, what, where_to_read)
}

/// The same notice for an interpreter.
fn python_notifier(ctx: &ToolContext, tool: &str, name: &str) -> bash::BackgroundNotifier {
    let what = format!("with `{tool}` in `{name}`");
    let where_to_read = format!("`check_python` with name `{name}` collects it");
    notice_background_end(ctx, what, where_to_read)
}

/// One notice, whichever tool started the work.
///
/// # Invariant
///
/// Queued as an `inject` marked `background`, and never raised while the session is
/// closing or gone: this is the server speaking, and a deleted session owes nothing.
fn notice_background_end(
    ctx: &ToolContext,
    what: String,
    where_to_read: String,
) -> bash::BackgroundNotifier {
    let store = ctx.store.clone();
    let session = ctx.session.clone();
    let closing = ctx.closing.clone();
    Arc::new(move |end: bash::BackgroundEnd| {
        let status = match end.code {
            Some(0) => "finished successfully".to_string(),
            Some(c) => format!("finished with exit status {c}"),
            None => "finished, though its exit status could not be read".to_string(),
        };
        let job = end.job;
        // How long it ran is the one thing the call that started it could not say.
        let ran = crate::agent::spent(end.ran.as_millis() as u64);
        let message = format!(
            "The background job {job} you started {what} has {status} after {ran}. \
             What it printed is kept until you read it: {where_to_read}."
        );
        // A deleted session owes nothing, and its rows would be stranded.
        if matches!(store.get_session(&session), Ok(None)) {
            return;
        }
        // A shell ended by the server's own exit did not finish: it was stopped.
        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        // Marked: the server talking, so it must not wake an idle session. See
        // `docs/notes/server-commands.md`.
        if let Err(e) = store.add_pending_task(
            &session,
            "inject",
            &json!({"session": session, "message": message, "notice": "background"}),
        ) {
            tracing::warn!(
                session = %session, job,
                error = %e, "could not notice a finished background job"
            );
        }
    })
}

/// task it spawns takes that shell's lock to finish reading it.
///
/// See `docs/notes/server-tools-mod.md`.
async fn leave_running(
    ctx: &ToolContext,
    machine: Machine,
    name: &str,
    handle: &Arc<Mutex<Shell>>,
    description: Option<&str>,
    cap: usize,
    started: std::time::Instant,
    because: &str,
) -> ToolOutcome {
    let retired = ctx.replace_shell(machine, name, handle);
    if !retired {
        tracing::warn!(
            session = %ctx.session,
            shell = %name,
            "shell was replaced while its command was still running"
        );
    }
    let store = ctx.store.clone();
    let session = ctx.session.clone();
    let shells = ctx.shells.clone();
    // A cut on an endpoint is not spilled here: the file would be on this machine.
    let output_dir = (!ctx.on_endpoint(machine)).then(|| ctx.output_dir.clone());
    let closing = ctx.closing.clone();
    // Same naming rule, with `late` where the call id goes: that call has already returned.
    let label = spill_label(&session, name, description, "late");
    let owned = name.to_string();
    // One measurement, spelled twice: the call that returned, and the notice later.
    let waited = started.elapsed().as_millis() as u64;
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
        let text = crate::tools::bash::cap_output(
            crate::prompts::shared(),
            &text,
            cap,
            output_dir.as_deref(),
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
            None => "finished, though the exit status could not be read".to_string(),
        };
        // The total, not the wait: only now is there a number to give.
        let total = crate::agent::spent(started.elapsed().as_millis() as u64);
        let message = format!(
            "The command you started in shell `{owned}`, which outlived its {} wait \
             and was left running, has now {status} after {total}.\n\n{text}",
            crate::agent::spent(waited)
        );
        // Marked like any notice: the server speaking, not the operator. See
        // `docs/notes/server-session.md`.
        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Err(e) = store.add_pending_task(
            &session,
            "inject",
            // A notice about a command is that command's shell's, not the session's side.
            &json!({"message": message, "notice": "finished", "machine": machine.label()}),
        ) {
            tracing::warn!(session = %session, "could not queue a finished command: {e}");
        }
        // Read to the end: no teardown has this shell's command left to stop.
        if let Ok(set) = shells.lock() {
            set.forget_retired(&handle);
        }
    });
    ToolOutcome::ok(format!(
        "still running after {}: {because}, so the wait ended and it was handed to the \
         background. The command itself was not stopped -- it continues in the old shell \
         `{name}`, which is now detached from this session, and its output will be posted \
         to this session when it finishes. `{name}` is a fresh shell now, in the session's \
         working directory.{}",
        crate::agent::spent(waited),
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

/// The command a call asks for, as the line the shell will read.
///
/// # Invariant
///
/// A list is quoted word by word, so each word reaches the shell as one word
/// exactly as it was written: no splitting on spaces, no globbing, no expansion
/// of `$` or `~`. A string is the line itself, and the shell reads it as a line.
fn command_line(value: Option<&Value>) -> Result<String, String> {
    match value {
        Some(Value::String(text)) => {
            if text.trim().is_empty() {
                return Err("bash received an empty command".to_string());
            }
            Ok(text.clone())
        }
        Some(Value::Array(words)) => {
            if words.is_empty() {
                return Err("bash received an empty list of words".to_string());
            }
            let mut quoted = Vec::with_capacity(words.len());
            for word in words {
                let Some(word) = word.as_str() else {
                    return Err(format!(
                        "bash received a list whose entries must all be strings, and one \
                         was {kind}",
                        kind = json_type(word),
                    ));
                };
                // A NUL ends an argument for every program there is, so a word
                // holding one arrives as a different word. Refuse it.
                if word.contains('\0') {
                    return Err(
                        "bash received a word holding a NUL byte, which no program could \
                         receive: an argument ends at a NUL, so it would arrive truncated"
                            .to_string(),
                    );
                }
                quoted.push(bash::shell_quote(word));
            }
            Ok(quoted.join(" "))
        }
        Some(other) => Err(format!(
            "bash requires a `command` that is a string or a list of strings, and this one is \
             {kind}",
            kind = json_type(other),
        )),
        None => Err("bash requires a `command`".to_string()),
    }
}

/// The name the tool on the session's remote is called by.
const REMOTE_BASH: &str = "remote_bash";

/// What `bash` says it is: one shell per name, on this machine.
const LOCAL_BASH_DESCRIPTION: &str = "Run a shell command in a persistent Git Bash on this \
     machine -- the one the server runs on. Each `name` is a separate shell that keeps \
     its own working directory, exports and shell functions between calls, and stays \
     alive until you `del_bash` it; omit `name` to use `default`. So reusing one name is \
     how state accumulates -- inventing a name per command leaves one idle process behind \
     per command. A name is one shell: `remote_bash` shares this name space, so a name \
     the other machine already holds is refused rather than quietly becoming a second \
     process. Output is streamed as it is produced. Use `cd` to move around instead of \
     passing absolute paths every time. Very long output is capped: you get the beginning \
     and the end, and the full text is written to a file named in the result -- read that \
     file with `sed -n` or `grep`, do not cat it. With `background`, the command is \
     started and the call returns immediately; collect what it printed later with \
     `check_bash`.";

/// What `remote_bash` says it is: the same thing, on the session's remote endpoint.
const REMOTE_BASH_DESCRIPTION: &str = "`bash` on the session's remote endpoint -- \
     the machine `set_session --remote` named. Same `command`, `name`, `description`, \
     `timeout_ms` and `background`, and the same name space: a name the other machine \
     already holds is refused rather than becoming a second shell. Paths are that \
     machine's, and output a cut truncates is not spilled to a file. Two things are not \
     offered here: `detached`, whose process belongs to this machine's registry, and a \
     `target` that is not this session's peer.";

pub struct BashTool {
    /// Which machine this tool drives. One implementation, two names.
    pub machine: Machine,
    pub timeout: Duration,
    /// 0 disables the cap.
    pub max_output_chars: usize,
}

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        match self.machine {
            Machine::Local => "bash",
            Machine::Remote => REMOTE_BASH,
        }
    }

    fn description(&self) -> &'static str {
        match self.machine {
            Machine::Local => LOCAL_BASH_DESCRIPTION,
            Machine::Remote => REMOTE_BASH_DESCRIPTION,
        }
    }

    fn machine(&self, _args: &Value, _ctx: &ToolContext) -> Machine {
        self.machine
    }

    fn parameters(&self) -> Value {
        // Two machines start in two places: this one where the harness runs, the peer in
        // the session's directory.
        let name_note = match self.machine {
            Machine::Local => {
                "Whichever bash this call is about. Without `detached`: which persistent \
                 shell to run in, default `default` -- reusing a name is what carries \
                 `cd`, exports and shell functions from one call to the next. With \
                 `detached`: the name of the detached bash being created, required, and \
                 unique in the session -- it is what `del_bash` uses to end that process \
                 later."
            }
            Machine::Remote => {
                "Which shell to run in on the machine this session's directory is on, \
                 default `default`. Reusing a name is what carries `cd`, exports and \
                 shell functions from one call to the next; a name `bash` already holds \
                 is refused rather than becoming a second shell."
            }
        };
        let mut properties = json!({
            "name": {
                "type": "string",
                "description": name_note
            },
            "command": {
                "description": "What to run: a command line, or a list of words -- the \
                                 program and its arguments, one word each. A list needs no \
                                 quoting: each word reaches the shell exactly as it is \
                                 written, so an argument with a space in it needs no \
                                 escaping. A command line is for what only a shell can do \
                                 (pipes, redirects, `cd`), a list is for a program and \
                                 its arguments.",
                "anyOf": [
                    {"type": "string"},
                    {"type": "array", "items": {"type": "string"}}
                ]
            },
            "description": {
                "type": "string",
                "description": "What this call is for, in a few words -- it is read \
                                 right after the tool's name, so say what the run is \
                                 for, not what the command is (`check the state`, not \
                                 `bash`). Optional, and never executed: it also names \
                                 the file that a too-long output is spilled to, so that \
                                 later you (and whoever reads the dumps) can tell one \
                                 run from another. Example: `run the server tests`."
            },
            "timeout_ms": {
                "type": "integer",
                "description": "How long to wait for the command before returning. \
                                 This is a limit on waiting, not on the command: when \
                                 it passes, the command keeps running in the background \
                                 and its output is posted to this session when it \
                                 finishes. Ignored for background commands."
            }
        });
        // A job of a shell is that shell's on either machine; only `bash` declares one.
        properties["background"] = json!({
            "type": "boolean",
            "description": "Start the command and return immediately instead of waiting \
                             for it. Collect what it printed with `check_bash`, and this \
                             session is told when it ends. Defaults to false."
        });
        match self.machine {
            Machine::Local => {
                properties["detached"] = json!({
                    "type": "boolean",
                    "description": "Run the command as a process of its own and return at \
                                     once. It keeps running when the shell is discarded, \
                                     when the session is cancelled and even after the server \
                                     exits -- that is how you restart the server you are \
                                     running on. Nothing is collected: its output is written \
                                     to the log file named in the result. Stop it later with \
                                     `del_bash` with its name stops it. Defaults to false."
                });
            }
            Machine::Remote => {
                properties["target"] = json!({
                    "type": "string",
                    "description": "Which machine to run on. Omit it: this session's \
                                     directory is on one peer, and that peer is where \
                                     `remote_bash` already runs. The name is accepted so \
                                     that writing it changes nothing."
                });
            }
        }
        json!({
            "type": "object",
            "properties": properties,
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
            let command = match command_line(args.get("command")) {
                Ok(command) => command,
                Err(message) => return ToolOutcome::err(message),
            };
            // `target` names the machine; another peer's must not quietly run here. See
            // `docs/notes/server-tools-mod.md`.
            if self.machine == Machine::Remote
                && let Some(stated) = args
                    .get("target")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                && ctx.remote.as_deref() != Some(stated)
            {
                return ToolOutcome::err(match &ctx.remote {
                    Some(here) => format!(
                        "`remote_bash` ran nothing: this session's directory is on remote \
                         {here:?}, and its shells are there -- {stated:?} is another \
                         machine, which this session has no directory on. A session works \
                         on one machine: `set_session --remote {stated}` moves it there, \
                         and ends these shells first."
                    ),
                    None => format!(
                        "`remote_bash` ran nothing: this session names no remote, so it has \
                         no peer to run on, and {stated:?} is not one it can reach from here."
                    ),
                });
            }
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_BASH);
            // Only ever a title: a missing or odd one costs an untitled row and a less
            // informative path, not a failed call.
            let description = args
                .get("description")
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
                // A detached process runs here, on the machine that holds the registry.
                if self.machine == Machine::Remote {
                    return ToolOutcome::err(
                        "`remote_bash` has no `detached`: the process would be started on \
                         this machine and outlive everything you asked for on that one. \
                         Start it in the foreground, or with `&` and a log file on the \
                         remote.",
                    );
                }
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
                let cwd = ctx.dir(Machine::Local);
                if ctx.find(name).is_some() {
                    return ToolOutcome::err(format!(
                        "this session already has a persistent shell named {name:?}; \
                         `del_bash name_regex: \"^{name}$\"` discards it first"
                    ));
                }
                return match ctx.detached.start(&ctx.session, name, &command, &cwd).await {
                    Ok(job) => ToolOutcome::ok(format!(
                        "started detached bash `{}` (pid {}), running in {}. It outlives the \
                         shell, the session, `cancel` and even a server restart, so it is how \
                         you replace the server you are running on. Delete it with \
                         `del_bash name_regex: \"^{}$\"`, which stops the process. Its output \
                         is not collected -- read it at: {}",
                        job.name,
                        job.pid,
                        cwd.display(),
                        job.name,
                        job.log
                    )),
                    Err(e) => ToolOutcome::err(format!("{e}")),
                };
            }
            // SPEC's key `{session, bash_name}` names one bash, not two.
            if ctx.detached.holds(&ctx.session, name).await {
                return ToolOutcome::err(format!(
                    "this session already has a detached bash named {name:?}; end it with \
                     `del_bash name_regex: \"^{name}$\"` or name this shell otherwise"
                ));
            }
            if background {
                let shell = match ctx.shell(self.machine, name) {
                    Ok(s) => s,
                    Err(e) => return ToolOutcome::err(format!("{e}")),
                };
                let mut shell = shell.lock().await;
                let notify = background_notifier(ctx, name);
                return match shell.run_background(&command, Some(notify)).await {
                    Ok(job) => ToolOutcome::ok(format!(
                        "started background job {job} in shell {name}; \
                         run `check_bash` to collect its output, and this session is told \
                         when it ends"
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

            let handle = match ctx.shell(self.machine, name) {
                Ok(s) => s,
                Err(e) => return ToolOutcome::err(format!("{e}")),
            };

            // Relay output as it arrives: draining after the command would block a chatty shell.
            let (tx, mut rx) = mpsc::channel::<BashChunk>(256);
            let mut shell = handle.lock().await;
            // Scoped so the future borrowing `shell` drops before the guard does.
            let started = std::time::Instant::now();
            let outcome = {
                let execute = shell.run(&command, timeout, Some(&tx));
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
            if matches!(&outcome, Err(e) if e.is::<WaitingMessage>()) {
                // Already streamed or queued in `rx`: clear the pump so a late
                // completion reports only output after this call.
                let _ = shell.take_partial_output();
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
                        self.machine,
                        name,
                        &handle,
                        description,
                        self.max_output_chars,
                        started,
                        "its deadline passed",
                    )
                    .await
                }
                Err(e) if e.is::<WaitingMessage>() => {
                    leave_running(
                        ctx,
                        self.machine,
                        name,
                        &handle,
                        description,
                        self.max_output_chars,
                        started,
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
                        let replaced = ctx.replace_shell(self.machine, name, &handle);
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
                    let text =
                        ctx.cap(self.machine, name, description, text, self.max_output_chars);
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
                    let replaced = ctx.replace_shell(self.machine, name, &handle);
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
                    let ended = e.is::<crate::tools::bash::ShellEnded>();
                    let replaced = !healthy && ctx.replace_shell(self.machine, name, &handle);
                    if !ended {
                        ToolOutcome::err(format!("{e}"))
                    } else {
                        // Named as a death, and what became of the shell is said.
                        ToolOutcome::err(format!(
                            "{e}; {}",
                            if replaced {
                                format!(
                                    "`{name}` has been replaced with a fresh shell in the \
                                     session's working directory, so `cd` and exports from \
                                     earlier calls are gone"
                                )
                            } else {
                                format!("`{name}` could not be replaced")
                            }
                        ))
                    }
                }
            }
        })
    }
}

// ------------------------------- check_bash --------------------------------

pub struct CheckBashTool {
    pub max_output_chars: usize,
}

/// How often a waiting read asks whether the shell's jobs are done.
const WAIT_POLL: Duration = Duration::from_millis(100);
/// A wait is a tool call, not a turn: beyond this it is better to come back later.
const MAX_WAIT: Duration = Duration::from_secs(600);

impl Tool for CheckBashTool {
    fn name(&self) -> &'static str {
        "check_bash"
    }

    fn description(&self) -> &'static str {
        "What a shell printed that you have not read: everything its `background` \
         commands wrote, plus anything it printed while nobody was reading. Reading \
         clears it, so each chunk comes back once. Empty means nothing yet -- ask \
         again, or set `wait_ms` to wait for the running jobs. A name is one shell on \
         whichever machine holds it, and the answer says which."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Shell to read. Defaults to `default`."
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "If this shell has background jobs, wait up to this \
                                    many milliseconds for them to finish before reading. \
                                    A timeout returns the output so far and leaves the jobs \
                                    running; it never stops them. Defaults to 0, which \
                                    reads now."
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
            let Some((machine, handle)) = ctx.find(name) else {
                return ToolOutcome::err(format!(
                    "no live shell named {name}; `list_bash` says which names exist"
                ));
            };
            let wait = args
                .get("wait_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(Duration::ZERO)
                .min(MAX_WAIT);
            if !wait.is_zero() {
                let deadline = std::time::Instant::now() + wait;
                loop {
                    let jobs = handle.lock().await.background_jobs();
                    if jobs == 0 {
                        break;
                    }
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(remaining.min(WAIT_POLL)) => {}
                        _ = ctx.cancel.cancelled() => {
                            return ToolOutcome::err(format!(
                                "check_bash stopped waiting for {name}; its background jobs \
                                 are still running"
                            ));
                        }
                    }
                }
            }
            let mut shell = handle.lock().await;
            let text = ctx.cap(
                machine,
                name,
                None,
                shell.take_pending(),
                self.max_output_chars,
            );
            let jobs = shell.background_jobs();
            let where_it_is = format!("{name} on {}", machine.label());
            if text.is_empty() {
                return ToolOutcome::ok(if jobs == 0 {
                    format!("{where_it_is}: no unread output")
                } else {
                    format!("{where_it_is}: {jobs} job(s) still running, no output yet")
                });
            }
            ToolOutcome::ok(format!(
                "{text}\n[{where_it_is}: {} job(s) still running]",
                jobs
            ))
        })
    }
}

// ----------------------------------- wait -----------------------------------

/// SPEC's `wait`: stay in this turn until something lands for this session.
///
/// # Invariant
///
/// It consumes nothing it waited for: every arrival is delivered at the next step
/// boundary, so a tool result is never a second copy of a message.
pub struct WaitTool {
    /// The ceiling one call waits, when the call names none.
    pub timeout: Duration,
}

impl Tool for WaitTool {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn description(&self) -> &'static str {
        "Stay in this turn until something arrives for this session: the report of \
         a background job that finished, the news of a `subscribe` whose pattern \
         matched, or a message from a person. It consumes nothing -- whatever \
         arrived is delivered at the next step boundary, the same way every other \
         queued message is -- so this call is how an agent waits without giving up \
         its turn, and without polling. With no `timeout_ms` it waits for the same \
         ceiling a `bash` call gets; when that passes it says nothing arrived, which \
         is not an error."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "timeout_ms": {
                    "type": "integer",
                    "description": "Give up after this many milliseconds. This is a \
                                    limit on waiting, not on the news: passing it \
                                    returns what has not arrived, and nothing is lost."
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
            let wait = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(self.timeout)
                .max(Duration::from_millis(100));
            let deadline = std::time::Instant::now() + wait;
            loop {
                // A person's message is the one arrival no row shows yet.
                if ctx.inbox.arrived() {
                    return ToolOutcome::ok(
                        "a message arrived for this session while this call waited; it is \
                         delivered at the next step boundary",
                    );
                }
                match ctx.store.pending_injects_of(&ctx.session) {
                    Ok(rows) if !rows.is_empty() => {
                        let mut what: Vec<String> = rows.iter().map(arrival).collect();
                        what.dedup();
                        return ToolOutcome::ok(format!(
                            "{} row(s) arrived while this call waited ({}); they are \
                             delivered at the next step boundary",
                            rows.len(),
                            what.join(", ")
                        ));
                    }
                    Ok(_) => {}
                    // An unreadable inbox is said, not waited out: the retry would
                    // see the same failure for the whole ceiling.
                    Err(e) => return ToolOutcome::err(format!("could not read the inbox: {e}")),
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return ToolOutcome::ok(format!(
                        "nothing arrived within {}; `wait` again, or get on with the work                          and this session will be told when it is",
                        crate::agent::spent(wait.as_millis() as u64)
                    ));
                }
                tokio::select! {
                    _ = tokio::time::sleep(remaining.min(WAIT_POLL)) => {}
                    _ = ctx.cancel.cancelled() => {
                        return ToolOutcome::err("wait stopped: this turn was cancelled");
                    }
                }
            }
        })
    }
}

/// What one queued row is, in the few words a summary can carry.
fn arrival(row: &crate::store::PendingTask) -> String {
    if row.payload.get("subscription").is_some() {
        return "a `subscribe` match".to_string();
    }
    match row.payload.get("notice").and_then(Value::as_str) {
        Some(kind) => format!("notice `{kind}`"),
        None => match row.payload.get("context_notice") {
            Some(_) => "a context notice".to_string(),
            None => "a message".to_string(),
        },
    }
}

// -------------------------------- list_bash --------------------------------

pub struct ListBashTool;

impl Tool for ListBashTool {
    fn name(&self) -> &'static str {
        "list_bash"
    }

    fn description(&self) -> &'static str {
        "List the bash of this session: their names, the machine each runs on, their \
         working directories, how much unread output they are holding, and how many \
         background jobs are still running. Also lists the session's detached bashes, \
         which are not any shell's and survive them all. `name_regex` narrows the \
         answer to the names that match, which is how one name's state is asked about."
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
            for (machine, name) in ctx.shell_names() {
                if !keep(&name) {
                    continue;
                }
                let Some(handle) = ctx.peek(machine, &name) else {
                    continue;
                };
                let shell = handle.lock().await;
                rows.push(json!({
                    "name": name,
                    "machine": machine.label(),
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
        "Delete bash whose name matches a regex, either kind. A persistent shell \
         is discarded: its working directories, exports and shell functions are \
         lost, and naming it again starts a fresh shell in its own machine's session \
         directory; the background jobs it was running go with it. A name is one shell, \
         so this ends it wherever it lives -- this machine or the session's remote. A \
         detached bash is stopped, which is the only thing that can -- its log file \
         stays where it is. Use this when a shell is wedged or polluted, and to clean \
         up the names a long session invented."
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
                .map(|(_, name)| name)
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
            let mut unconfirmed = Vec::new();
            for name in &names {
                // A timeout left the old command under the name: end both.
                for shell in ctx.discard(name) {
                    let report = shell.end().await;
                    if !report.confirmed {
                        unconfirmed.push(format!(
                            "{name}: {}",
                            report.unknown.unwrap_or_else(|| "unknown".to_string())
                        ));
                    }
                }
                discarded.push(name.clone());
            }
            let list = discarded.join(", ");
            let unconfirmed = if unconfirmed.is_empty() {
                String::new()
            } else {
                format!(
                    "; the endpoint did not confirm ending {}",
                    unconfirmed.join("; ")
                )
            };
            let kept = if stopped.stopped > 0 {
                format!("; also deleted {} detached bash(es)", stopped.stopped)
            } else {
                String::new()
            };
            ToolOutcome::ok(format!("discarded shell(s) {list}{kept}{unconfirmed}"))
        })
    }
}

/// How a spill file is named: `session-shell-description-callid`, the same for both paths.
fn spill_label(session: &str, shell: &str, description: Option<&str>, call_id: &str) -> String {
    match description {
        Some(description) => format!("{session}-{shell}-{description}-{call_id}"),
        None => format!("{session}-{shell}-{call_id}"),
    }
}

/// SPEC's per-call `max_output`: over the cap the output spills, and the
/// call answers with the file's metadata.
///
/// # Invariant
///
/// `None` is "no cap": under it, unparseable, or on an endpoint session,
/// where a file here is not one its tools can open.
pub(crate) fn cap_max_output(
    ctx: &ToolContext,
    tool: &str,
    description: Option<&str>,
    call_id: &str,
    text: &str,
    max: usize,
) -> Option<String> {
    if max == 0 || text.chars().count() <= max {
        return None;
    }
    if ctx.on_endpoint(ctx.machine) {
        tracing::warn!(
            session = %ctx.session,
            tool,
            "max_output cannot spill on an endpoint session; the cap is left unhonored"
        );
        return None;
    }
    let label = spill_label(&ctx.session, tool, description, call_id);
    let path = crate::tools::bash::spill(&ctx.output_dir, &label, text)?;
    Some(
        json!({
            "path": path,
            "chars": text.chars().count(),
            "line_count": text.lines().count(),
        })
        .to_string(),
    )
}

/// The shell a tool call is about, or `default`.
fn shell_name(args: &Value) -> &str {
    args.get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BASH)
}
