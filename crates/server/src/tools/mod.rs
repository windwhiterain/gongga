//! Tools the model calls, and the one clock every call runs on.
//!
//! See `docs/notes/server-tools-mod.md`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use protocol::ToolAnswerEvent;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::session::{Machine, Workspace};
use crate::store::Store;

pub mod channel;
pub mod detached;
pub mod exec;
#[cfg(windows)]
pub mod job;
pub mod laws;
pub mod live_compact;
pub mod memory;
pub mod skill;
pub mod spec;
pub mod spill;
pub mod words;

/// Everything a tool invocation needs, supplied per call by the agent loop.
///
/// # Invariant
///
/// Copyable, because a call that said `background` outlives the call that made it:
/// it runs in a process of its own, with no stream of the turn's to write to.
#[derive(Clone)]
pub struct ToolContext {
    pub session: String,
    pub call_id: String,
    /// The name this call was made under, so a message can say which tool it was.
    ///
    /// # Invariant
    ///
    /// The row's name, not a constant in the command: a tool renamed in the config
    /// says the new name in every message it writes.
    pub tool: String,
    /// The home this call belongs to: what a worker needs to be started.
    pub home: PathBuf,
    /// Where capped tool output is spilled, so nothing is lost by capping.
    pub output_dir: PathBuf,
    /// The channels: programs this session started, by the name it calls them.
    pub channels: Arc<crate::tools::channel::Channels>,
    /// The session's named shells, on this machine and -- when it names one --
    /// on its remote. See `crate::session::Shells`.
    pub workspace: Arc<StdMutex<Workspace>>,
    /// The session's one path space, holding the connection a peer is read through.
    ///
    /// # Invariant
    ///
    /// Cloned from the session, never built here: a fresh space per call would dial the
    /// peer afresh, and a message naming twelve files would cost twelve dials.
    pub space: crate::space::Space,
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
    /// Streaming sink for `tool_answer` events.
    pub events: Option<mpsc::Sender<ToolAnswerEvent>>,
    pub cancel: CancellationToken,
    /// Where a command that outlived its deadline reports back: an inbox row, not a result.
    pub store: Store,
    /// The session's working directory, for tools that take a relative path.
    pub cwd: PathBuf,
    /// What a program this turn starts is given. See `SessionHandle::env`.
    pub env: Vec<(String, String)>,
    /// What this turn's model can take in; a picture tool asks before sending one.
    pub vision: crate::llm::Vision,
    /// Whether a message arrived for this session that its turn cannot read yet.
    pub inbox: crate::session::InboxSignal,
    /// Whether a `cancel` is ending this turn, said where a tool call can hear it.
    pub stop: crate::session::StopSignal,
    /// Set when shells end because the server is going: nothing that stops then finished.
    pub closing: Arc<std::sync::atomic::AtomicBool>,
    /// Room left below which a finished call reports, as the config states it now;
    /// 0 is off. See `crate::disk`.
    pub disk_low: u64,
    /// The look this session takes after a program call, when it reads. `None` is work.
    ///
    /// # Invariant
    ///
    /// One field for both facts, because they are one fact: a session that may not
    /// change its workspace is a session that looks, and `is_some` is the whole test
    /// a tool is refused or a look is taken by. See `docs/notes/server-explore.md`.
    pub explore: Option<std::sync::Arc<crate::explore::Watch>>,
}

impl ToolContext {
    /// Whether the session's work on `machine` is somewhere other than here.
    pub(crate) fn on_endpoint(&self, machine: Machine) -> bool {
        self.workspace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_remote(machine)
    }

    /// The path space this call's paths live in: where the session stands.
    ///
    /// # Invariant
    ///
    /// One space per session, because a session works on one machine: a tool takes a
    /// path in this space and asks nothing about whether it is a peer. See
    /// `docs/notes/server-space.md`.
    pub(crate) fn space(&self) -> crate::space::Space {
        self.space_on(self.machine)
    }

    /// The space one of this session's two machines spells paths in.
    ///
    /// # Invariant
    ///
    /// `machine` is the one that produced the text being read, never inferred from the
    /// path's spelling: a tool's output is read where the tool ran, a message where the
    /// session stands. The session's own machine is its held space, connection and all;
    /// the other machine is this one, which needs none.
    pub(crate) fn space_on(&self, machine: Machine) -> crate::space::Space {
        match machine == self.machine {
            true => self.space.clone(),
            false => crate::space::Space::local(),
        }
    }

    /// Where `machine` starts: the session's directory, empty on the other machine.
    pub(crate) fn dir(&self, machine: Machine) -> PathBuf {
        self.workspace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dir(machine)
    }

    /// The session's own directory, on whichever machine owns it.
    ///
    /// # Invariant
    ///
    /// The one directory a reference that names a machine out loud is joined against:
    /// a message's `@image(remote://p/x)` stands in the session's directory as the peer
    /// spells it, never in whatever directory the tool that printed it ran in.
    pub(crate) fn session_dir(&self) -> PathBuf {
        self.cwd.clone()
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
        // An answer on an endpoint session is not spilled here: a path on this machine
        // is not a path the reader can open.
        let dir = (!self.on_endpoint(machine)).then_some(self.output_dir.as_path());
        crate::tools::spill::cap_output(crate::prompts::shared(), &text, max, dir, &label)
    }
}

/// Where a program this machine starts stands, when the session cannot say.
///
/// # Invariant
///
/// A directory this machine does not have never fails a call. Empty is a session whose
/// directory is on a peer, and a path that is not a directory is one gone: both answer
/// this server's own. A stated directory is its caller's to refuse, by name.
pub(crate) fn standing_dir(cwd: &std::path::Path) -> PathBuf {
    if !cwd.as_os_str().is_empty() && cwd.is_dir() {
        return cwd.to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
    fn name(&self) -> &str;

    /// The row this tool is: its arguments, its sentences and every flag below.
    ///
    /// # Invariant
    ///
    /// By value because a row that forwards to a declared function is re-stated by that
    /// function's own answer as it is read: the table is filled when the content is first
    /// read, so a row renders the same arguments the catalog published. See `Specd::spec`.
    fn spec(&self) -> spec::ToolSpec;

    /// Which machine this call's paths belong to.
    ///
    /// # Invariant
    ///
    /// A shell tool answers for the machine it drives; every other tool answers for
    /// where the session stands, unless its own arguments name a machine. That is
    /// the whole of "whose file is this".
    fn machine(&self, _args: &Value, ctx: &ToolContext) -> Machine;

    /// Whether the model is offered this tool at all: a row may say it is not.
    fn offered(&self) -> bool;

    /// Whether this tool drives the session's remote.
    ///
    /// # Invariant
    ///
    /// The tool answers for itself, so a twin added tomorrow cannot be offered to a
    /// session that has no such machine. See `definitions`.
    fn is_remote(&self) -> bool;

    /// Whether calling this tool is itself a change to the workspace.
    ///
    /// # Invariant
    ///
    /// Said by the tool that knows, not inferred from its name: a session that is for
    /// a read refuses exactly the tools whose arguments *are* the write, since no
    /// earlier or later look could catch what has not happened yet. A tool that
    /// changes the workspace some other way -- through a program -- is caught by the
    /// look its call takes when it ends. See `docs/notes/server-explore.md`.
    fn writes(&self) -> bool;

    /// The command this tool hands its call to, when that is all it does.
    ///
    /// # Invariant
    ///
    /// `Some(name)` says the row is that command's spelling, not a second implementation:
    /// the catalog publishes the command and not this alias, so a name whose command is
    /// gone is unknown rather than a call that resolves back to itself for ever.
    fn forwards_to(&self) -> Option<&str> {
        None
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
}

/// What a config row's `cmd` names: the one implementation that does the work.
///
/// # Invariant
///
/// Nothing about a tool is here but the work. The name, the arguments, the sentences
/// and every flag are the row's ([`spec::ToolSpec`]), so two rows naming one command
/// are two tools with two schemas -- `bash` and `remote_bash` are that pair.
pub trait Cmd: Send + Sync {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;

    /// The machine a call's own arguments name, when they name one.
    ///
    /// # Invariant
    ///
    /// `None` is the row's: a command that runs where the session stands says nothing
    /// here. A file command answers, because `--remote` is an argument of the call and
    /// not a fact about the tool -- which no config row could state.
    fn machine(&self, _args: &Value, _ctx: &ToolContext) -> Option<Machine> {
        None
    }

    /// The command this one hands its call to, when that is all it does.
    fn forwards_to(&self) -> Option<&str> {
        None
    }
}

/// SPEC's `fn <tool>(**args)`: one command, called as a tool.
///
/// # Invariant
///
/// The command layer resolves the name when the call is made, so a row may offer a
/// `ServerScript`, a skill's entry or any other command with no Rust type behind it, and
/// a name nobody answers is refused there -- in the command's own words, rather than as a
/// missing tool. It must never leave as a worker: a worker has no command layer.
pub struct Forward {
    name: String,
}

impl Cmd for Forward {
    fn forwards_to(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let words = match args.as_object() {
                Some(table) => named_words(table),
                None => protocol::Args::new(),
            };
            run_command(&ctx.session, &self.name, words).await
        })
    }
}

/// SPEC's `fn gongga_cli(cmd: Cmd)`: the route whose call names the command it runs.
///
/// # Invariant
///
/// The same door as [`Forward`], named by the call instead of by the row: one command
/// layer, the caller's session riding along, and a name nobody answers refused there in
/// the command's own words. `cmd` is SPEC's `{name, args}` -- the object, or the words
/// of one, and anything else is refused rather than guessed at. A worker has no command
/// layer, so this never leaves as one.
pub struct GonggaCli;

impl Cmd for GonggaCli {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let (name, words) = match route(&args) {
                Ok(route) => route,
                Err(refusal) => return ToolOutcome::err(refusal),
            };
            run_command(&ctx.session, &name, words).await
        })
    }
}

/// SPEC's `Cmd` as one call's `cmd` argument: the command's name and its words.
///
/// # Invariant
///
/// The command's own arguments are read by the rule every command's are ([`words_of`]),
/// and `args` is optional because a command need take none. See
/// `docs/notes/server-tools-mod.md`.
fn route(args: &Value) -> Result<(String, protocol::Args), String> {
    // Said once: the two refusals below are the same sentence about the same shape.
    let shape = "`cmd` is SPEC's `Cmd`: `cmd.name`, the command, and `cmd.args`, its arguments";
    let given = args.get("cmd");
    let Some(table) = given.and_then(object_of) else {
        return Err(format!(
            "`gongga_cli` did not run: {shape}. This call gave {}.",
            given.map_or("none", json_type)
        ));
    };
    let name = match table.get("name") {
        Some(Value::String(name)) if !name.trim().is_empty() => name.clone(),
        _ => {
            return Err(format!(
                "`gongga_cli` did not run: {shape}, and this one names neither."
            ));
        }
    };
    let words = match table.get("args") {
        None | Some(Value::Null) => protocol::Args::new(),
        Some(value) => match object_of(value) {
            Some(table) => words_of(&table),
            None => {
                return Err(format!(
                    "`gongga_cli` did not run: `{name}`'s `args` is an object, and this was {}.",
                    json_type(value)
                ));
            }
        },
    };
    Ok((name, words))
}

/// A value as the object it stands for: the object itself, or the words of one.
///
/// # Invariant
///
/// An operator's line spells the whole `cmd` as a string, and a call may spell an inner
/// table the same way, so the JSON of an object is that object. Nothing else is one.
fn object_of(value: &Value) -> Option<serde_json::Map<String, Value>> {
    match value {
        Value::Object(table) => Some(table.clone()),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(table)) => Some(table),
            _ => None,
        },
        _ => None,
    }
}

/// Runs one server cmd in the caller's session and answers with SPEC's `Result`.
///
/// # Invariant
///
/// SPEC's `fn <tool>(**args)`: the command layer is the whole door, so a name it does
/// not know is refused there in the command's own words, and the caller's session
/// rides along. Nothing races the command against the turn's token: only a wait may
/// be ended, so the command answers and the turn ends at the next step boundary. See
/// `docs/notes/server-tools-mod.md`.
pub(crate) async fn run_command(session: &str, name: &str, words: protocol::Args) -> ToolOutcome {
    let Some(layer) = crate::commands::layer() else {
        return ToolOutcome::err(format!(
            "{name} is a command, and this process has no command layer: it runs one call and answers with a file"
        ));
    };
    let began = std::time::Instant::now();
    let done = layer.run(session, name, words).await;
    let rows = answer_rows(&done.events);
    let mut answer = json!({
        "id": Value::Null,
        "output": done.message,
        "time": format!("{:.3}s", began.elapsed().as_secs_f64()),
        "matched": false,
        "exit": if done.ok { 0 } else { 1 },
        "data": done.data,
    });
    if !rows.is_empty() {
        answer["events"] = Value::Array(rows);
    }
    if done.ok {
        ToolOutcome::ok(answer.to_string())
    } else {
        ToolOutcome::err(answer.to_string())
    }
}

/// The rows an answer carries, in the spelling its connection would have sent them.
///
/// # Invariant
///
/// The event and its number, never a rendering of it: a caller that parses them and a
/// window that draws them are reading one shape. See `docs/notes/server-tools-mod.md`.
fn answer_rows(events: &[(Option<u64>, protocol::Event)]) -> Vec<Value> {
    events
        .iter()
        .map(|(seq, event)| json!({"seq": seq, "event": event}))
        .collect()
}

/// A call's arguments as the arguments a command reads.
///
/// # Invariant
///
/// The value the call stated, never its text: SPEC's `Cmd.args` is `dict[str, any]`, so
/// `wait` may be a number, `config` a table, and a stated `null` the type's empty value
/// rather than the four letters. See `protocol::Args` and `protocol::argtype`.
pub(crate) fn words_of(table: &serde_json::Map<String, Value>) -> protocol::Args {
    let mut words = protocol::Args::new();
    for (key, value) in table {
        words.insert(key.clone(), value.clone());
    }
    words
}

/// The same for a call whose `cmd` names the command rather than one of its arguments.
///
/// # Invariant
///
/// `cmd` is the one key this drops: a row names its command under that key, so the value
/// there is never that command's own. See `docs/notes/server-tools-mod.md`.
pub(crate) fn named_words(table: &serde_json::Map<String, Value>) -> protocol::Args {
    let mut words = words_of(table);
    words.remove("cmd");
    words
}

/// One row of the config, offered as the tool the model and the operator call.
pub struct Specd {
    /// The row's name: the key it is declared under.
    name: String,
    /// The row: everything said about this tool, less what its function states.
    row: spec::ToolSpec,
    /// The commands a package declares, and the arguments each one's function takes.
    declared: Declared,
    /// The command the row names, which does the work.
    work: Box<dyn Cmd>,
}

impl Specd {
    pub fn new(name: String, row: spec::ToolSpec, declared: Declared, work: Box<dyn Cmd>) -> Self {
        Self {
            name,
            row,
            declared,
            work,
        }
    }
}

impl Tool for Specd {
    fn name(&self) -> &str {
        &self.name
    }

    fn spec(&self) -> spec::ToolSpec {
        let declared = self.declared.read().unwrap_or_else(|e| e.into_inner());
        match declared.get(&self.row.cmd) {
            Some(given) => self.row.clone().from_declared(given),
            None => self.row.clone(),
        }
    }

    fn machine(&self, args: &Value, ctx: &ToolContext) -> Machine {
        self.work
            .machine(args, ctx)
            .unwrap_or_else(|| self.row.machine.machine())
    }

    fn offered(&self) -> bool {
        self.row.offered
    }

    fn is_remote(&self) -> bool {
        self.row.machine == spec::Side::Remote
    }

    fn writes(&self) -> bool {
        self.row.writes
    }

    fn forwards_to(&self) -> Option<&str> {
        self.work.forwards_to()
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        self.work.run(args, ctx)
    }
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    /// What a command needs that no config row states.
    settings: Settings,
}

/// The arguments each declared command's function takes, by the command's name.
///
/// # Invariant
///
/// One table, written where the content is read and read where a row is rendered: a tool
/// row that forwards to a declared function states no arguments of its own, so what the
/// model is offered, what a call is checked against and what an operator completes all
/// come from the function's signature. Empty until the content is first read.
pub type Declared =
    std::sync::Arc<std::sync::RwLock<std::collections::BTreeMap<String, Vec<spec::Argument>>>>;

/// What this build hands a command the config names.
///
/// # Invariant
///
/// Not a tool and not a row: the numbers and the programs a command is built with,
/// which belong to the machine and not to what the model is told.
struct Settings {
    max_output_chars: usize,
    /// The declared arguments every forwarding row derives from; empty until read.
    declared: Declared,
    python: Option<(crate::config::PythonConfig, Option<std::path::PathBuf>)>,
    skills: Option<skill::Skills>,
}

impl ToolRegistry {
    /// A registry with no tools yet: the rows come from the config, in [`Self::build`].
    ///
    /// # Invariant
    ///
    /// Nothing is registered here. What a build carries is what the home's `[tools]`
    /// declares, so a row added in the config is a tool and a row removed is not.
    pub fn settings(max_output_chars: usize) -> Self {
        Self {
            tools: Vec::new(),
            settings: Settings {
                max_output_chars,
                python: None,
                skills: None,
                declared: Default::default(),
            },
        }
    }

    /// The table a forwarding row derives its arguments from, filled when the content is read.
    pub fn with_declared(mut self, declared: Declared) -> Self {
        self.settings.declared = declared;
        self
    }

    /// The interpreter a `python` row runs in, and where its package import lives.
    pub fn with_python(
        mut self,
        python: crate::config::PythonConfig,
        lib: Option<std::path::PathBuf>,
    ) -> Self {
        self.settings.python = Some((python, lib));
        self
    }

    /// What this build runs a skill with: the interpreter, the home's import, the bounds.
    pub fn with_skills(
        mut self,
        python: Option<std::path::PathBuf>,
        lib: Option<std::path::PathBuf>,
        timeout: Duration,
    ) -> Self {
        self.settings.skills = Some(skill::Skills {
            python,
            lib,
            timeout,
        });
        self
    }

    /// The skills this build can run; `None` before a config resolved them.
    ///
    /// # Invariant
    ///
    /// The setting and not a row: `run_skill` runs a skill whether or not any tool is
    /// registered for it, so what a skill is started with must outlive the table.
    pub fn skills(&self) -> Option<&skill::Skills> {
        self.settings.skills.as_ref()
    }

    /// The table a forwarding row derives its arguments from.
    pub fn declared(&self) -> &Declared {
        &self.settings.declared
    }

    /// Builds one tool per row the config declares.
    ///
    /// # Invariant
    ///
    /// Every tool is a row, so the model's list, the operator's commands and what this
    /// process carries are one table. A row naming a command this build does not know,
    /// or one whose setting is missing, is not carried and the reason is kept in
    /// [`Self::problems`]: a tool that vanished quietly is a model calling what is not
    /// there.
    pub fn build(mut self, rows: &std::collections::BTreeMap<String, spec::ToolSpec>) -> Self {
        for (name, row) in rows {
            match self.command(row) {
                Ok(work) => self.tools.push(Arc::new(Specd::new(
                    name.clone(),
                    row.clone(),
                    self.declared().clone(),
                    work,
                ))),
                Err(why) => tracing::warn!(
                    row = %name,
                    "`[tool.{name}]` is not carried: {why}"
                ),
            }
        }
        // Said once: the rows the model reads.
        let offered: Vec<&str> = self
            .tools
            .iter()
            .filter(|tool| tool.offered())
            .map(|tool| tool.name())
            .collect();
        tracing::info!(tools = ?offered, "tools the model is offered");
        self
    }

    /// The command a row names, built with this build's own settings.
    fn command(&self, row: &spec::ToolSpec) -> Result<Box<dyn Cmd>, String> {
        Ok(match row.cmd.as_str() {
            "wait" => Box::new(WaitTool {
                timeout: WAIT_DEFAULT,
            }),
            "exec" => {
                // The channels and default wait are the tool's; the cap is the call
                // layer's, so `max_output` can raise it.
                Box::new(exec::ExecTool {
                    wait: exec::Wait {
                        default: exec::DEFAULT_WAIT,
                    },
                })
            }
            "live_compact" => Box::new(live_compact::LiveCompactTool),
            "memorize" => Box::new(memory::MemorizeTool),
            "recall" => Box::new(memory::RecallTool),
            // SPEC's `fn gongga_cli(cmd: Cmd)`: a call that names its own command.
            "gongga_cli" => Box::new(GonggaCli),
            // SPEC's `fn <tool>(**args)`: a row with no type here runs through the
            // command layer, offered to the model or not.
            other => Box::new(Forward {
                name: other.to_string(),
            }),
        })
    }

    /// Every tool, in registration order: the catalog generator's one source.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Tool> {
        self.tools.iter().map(|tool| tool.as_ref())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).map(Arc::clone)
    }

    /// Which machine a named tool's paths belong to.
    pub fn machine_of(&self, name: &str, args: &Value, ctx: &ToolContext) -> Machine {
        match self.get(name) {
            Some(tool) => tool.machine(args, ctx),
            None => ctx.machine,
        }
    }

    /// The tool set a session is stamped with, rendered once per stored change.
    ///
    /// # Invariant
    ///
    /// The only renderer of a schema, called only from a stored change -- never
    /// while a request is built. Checkpoint and memory tools are in every head,
    /// whatever the switch says. See `docs/notes/server-tools.md`.
    ///
    /// Every session is offered the same set, `remote` included: a call may name
    /// any machine, and one the server does not know is refused when it runs.
    pub fn stamp(&self) -> Vec<crate::llm::ToolDefinition> {
        self.tools
            .iter()
            .filter(|t| t.offered())
            .map(|t| Self::definition_of(t))
            .collect()
    }

    fn definition_of(tool: &Arc<dyn Tool>) -> crate::llm::ToolDefinition {
        let parameters = general_parameters(tool.as_ref());
        crate::llm::ToolDefinition {
            name: tool.name().to_string(),
            description: tool.spec().summary.clone().unwrap_or_default(),
            parameters,
        }
    }

    /// Runs one tool, and reads a failure as what it may be: a full disk.
    ///
    /// # Invariant
    ///
    /// Every tool call, from whatever door, goes through here.
    ///
    /// # Invariant
    ///
    /// One place runs a tool, so it is the one place the shared arguments are taken off a
    /// call: a model's, a script's and a command layer's are read the same way, and a new
    /// tool gets them without knowing their names. It is also where a failing tool is
    /// noticed -- what it notices is not the tool's own words but the room left where the
    /// harness writes: see `docs/notes/server-disk.md`.
    pub async fn run(self: &Arc<Self>, name: &str, args: Value, ctx: &ToolContext) -> ToolOutcome {
        // Shared names come off before anything reads the arguments, so no handler
        // answers for what it never declared.
        let mut args = args;
        let shared = laws::Given::take(&mut args);
        // The one cap, read here so a command call is held to it too: its branch returns
        // before any tool sees it.
        let cap = shared.cap(self.settings.max_output_chars);
        let Some(tool) = self.get(name) else {
            // SPEC `fn <tool>(**args)`: an unregistered name is a cmd a skill taught; the command layer runs it.
            let words = match args.as_object() {
                Some(table) => named_words(table),
                None => protocol::Args::new(),
            };
            let outcome = run_command(&ctx.session, name, words).await;
            return capped(ctx, name, &shared, cap, outcome);
        };
        // Before the check and before the tool: a typed line and a `hint = "json"` are text.
        let args = spelled_values(tool.as_ref(), args);
        // The name this call was made under, as the row spells it now.
        let mut named = ctx.clone();
        named.tool = name.to_string();
        let ctx = &named;
        // Before the arguments are read: a refused call must not reach the tool at all,
        // so nothing is spawned for it.
        if tool.writes() && ctx.explore.is_some() {
            return ToolOutcome::err(crate::explore::refusal(crate::prompts::shared(), name));
        }
        if let Some(message) = argument_error(tool.as_ref(), &args) {
            return ToolOutcome::err(message);
        }
        let outcome = invoke(name, tool, args, ctx).await;
        let outcome = capped(ctx, name, &shared, cap, outcome);
        // The call ended; the look is a task of its own, so nothing waits on it.
        crate::explore::after_job(ctx, name);
        outcome
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}

/// What a call says when its own work stopped without answering.
fn stopped(name: &str) -> ToolOutcome {
    ToolOutcome::err(format!(
        "`{name}` ended without answering: the work it was running stopped before it could          say how it went."
    ))
}

/// Runs one call, and ends its wait when somebody speaks or the turn stops.
///
/// # Invariant
///
/// The only wait, so "a message reaches the turn" is one rule and not one per tool.
/// The work runs on a task with a token of its own, so a message can end the wait
/// without stopping what the call is doing.
///
/// # Invariant
///
/// No clock of its own: how long a call waits is the call's to state (`exec`'s
/// `wait`, `wait`'s `time`), and a number here would end waits nobody asked to
/// end. See `docs/notes/server-tools-mod.md`.
async fn waited(tool: &Arc<dyn Tool>, args: &Value, ctx: &ToolContext) -> ToolOutcome {
    let started = std::time::Instant::now();
    let mut job = ctx.clone();
    job.cancel = CancellationToken::new();
    let token = job.cancel.clone();
    let work = Arc::clone(tool);
    let given = args.clone();
    let (tx, mut rx) = mpsc::channel::<ToolOutcome>(1);
    // Detached on purpose: what it is doing is not dropped when the wait ends.
    tokio::spawn(async move {
        let outcome = work.run(given, &job).await;
        let _ = tx.send(outcome).await;
    });
    let mut punctual = tokio::time::interval(INBOX_POLL);
    let why = loop {
        tokio::select! {
            answer = rx.recv() => return answer.unwrap_or_else(|| stopped(tool.name())),
            _ = ctx.cancel.cancelled() => break match ctx.stop.interrupted() {
                true => "interrupt",
                false => "cancel",
            },
            _ = punctual.tick() => {
                if ctx.inbox.arrived() {
                    break "message";
                }
            }
        }
    };
    tracing::info!(
        session = %ctx.session,
        tool = %tool.name(),
        why,
        elapsed = ?started.elapsed(),
        "the wait ended before the call answered"
    );
    // A waiting call returns early; its token says the wait is over. A call with no wait
    // to end answers on its own path.
    token.cancel();
    match rx.recv().await {
        Some(answer) => answer,
        None => stopped(tool.name()),
    }
}

/// A text value for an object-or-array argument is the value it spells; a string stays text.
pub(crate) fn spelled_values(tool: &dyn Tool, mut args: Value) -> Value {
    let schema = tool.spec().schema();
    let (Some(declared), Some(table)) = (schema["properties"].as_object(), args.as_object_mut())
    else {
        return args;
    };
    for (name, value) in table.iter_mut() {
        let Value::String(text) = value else { continue };
        let Some(property) = declared.get(name) else {
            continue;
        };
        let Ok(spelled) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        // A text that spells a string is the text itself: quoting is not a value.
        if kind_word(&spelled) == "string" {
            continue;
        }
        if kind_of(property)
            .split(" or ")
            .any(|kind| kind == kind_word(&spelled))
        {
            *value = spelled;
        }
    }
    args
}

/// The model-facing schema: what the row declares, plus the shared arguments.
///
/// # Invariant
///
/// The one place a schema is rendered for a model, so the shared arguments are added
/// here and nowhere else: [`laws`] holds the list, and every tool gets it whether its
/// row knows the names or not. See `docs/notes/server-tools-mod.md`.
pub(crate) fn general_parameters(tool: &dyn Tool) -> Value {
    laws::schema(tool.spec().schema())
}

/// The JSON kind of one value, in a schema's own vocabulary.
fn kind_word(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The first argument whose value is not of the JSON kind its schema declares, and the
/// refusal that says so.
///
/// # Invariant
///
/// The schema is the whole judgement: a type that does not list `null` refuses a stated
/// `null`, which is how "the same as omitting it" stops being something a reader has to
/// guess. See `protocol::argtype`.
fn wrong_kind(schema: &Value, given: &serde_json::Map<String, Value>) -> Option<String> {
    let properties = schema["properties"].as_object()?;
    for (key, value) in given {
        let Some(spec) = properties.get(key) else {
            continue;
        };
        let spelled = kind_of(spec);
        // `any` is a property that declares no type: nothing to hold the call to.
        if spelled == "any" {
            continue;
        }
        let kinds: Vec<&str> = spelled.split(" or ").collect();
        let given_kind = kind_word(value);
        let matches =
            kinds.contains(&given_kind) || (given_kind == "integer" && kinds.contains(&"number"));
        if !matches {
            return Some(format!(
                "`{key}` takes {}, and this call gave {}: leave it out for its default, or give a value of that kind.",
                kinds.join(" or "),
                json_type(value)
            ));
        }
    }
    None
}

/// Why a call does not fit the declared schema, in the schema's own words.
///
/// See `docs/notes/server-tools-mod.md`.
fn argument_error(tool: &dyn Tool, args: &Value) -> Option<String> {
    argument_error_for(tool.name(), &tool.spec().schema(), args)
}

/// The same, for a name and a schema: what is missing, and what the tool does not take.
///
/// # Invariant
///
/// A key the schema does not declare is named even when nothing is missing. `exec`
/// requires no argument -- `bin` starts a channel, `id` speaks to one -- so the only
/// thing wrong with `{"command": [...]}` is the key itself. Answering for missing
/// arguments alone returned `None` there, and the call's words were dropped in silence.
fn argument_error_for(name: &str, schema: &Value, args: &Value) -> Option<String> {
    let Some(given) = args.as_object() else {
        return Some(format!(
            "`{name}` did not run: its arguments must be a JSON object, and this was {kind}.\n  \
             `{name}` takes: {signature}",
            kind = json_type(args),
            signature = signature(schema),
        ));
    };

    let required = string_list(&schema["required"]);
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| given.get(*k).is_none_or(Value::is_null))
        .collect();

    let declared = string_list(&schema["properties"]);
    let unexpected: Vec<String> = given
        .keys()
        .filter(|k| {
            !declared.contains(&k.as_str())
                && !laws::ALL.iter().any(|(name, _, _)| *name == k.as_str())
        })
        .map(
            |wrong| match declared.iter().find(|d| is_shorthand_of(d, wrong)) {
                Some(right) => format!("`{wrong}` (did you mean `{right}`?)"),
                None => format!("`{wrong}`"),
            },
        )
        .collect();

    // Wrong kinds are refused here, in the schema own words. Named: a step may carry
    // several calls.
    if let Some(wrong) = wrong_kind(schema, given) {
        return Some(format!("`{name}` did not run: {wrong}"));
    }

    if missing.is_empty() && unexpected.is_empty() {
        return None;
    }

    let mut message = if missing.is_empty() {
        format!(
            "`{name}` did not run: {} {} not {} it takes.\n",
            unexpected.join(", "),
            if unexpected.len() == 1 { "is" } else { "are" },
            if unexpected.len() == 1 {
                "an argument"
            } else {
                "arguments"
            },
        )
    } else {
        format!(
            "`{name}` did not run: {} {} required.\n",
            missing
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", "),
            if missing.len() == 1 { "is" } else { "are" },
        )
    };
    if !missing.is_empty() && !unexpected.is_empty() {
        message.push_str(&format!("  unexpected: {}\n", unexpected.join(", ")));
    }
    message.push_str(&format!("  `{name}` takes: {}", signature(schema)));
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
    match spec.get("type") {
        Some(Value::String(one)) => one.clone(),
        // A union is a list: `optional[str]` renders as `["string", "null"]`.
        Some(Value::Array(kinds)) => {
            let kinds: Vec<&str> = kinds.iter().filter_map(Value::as_str).collect();
            if kinds.is_empty() {
                "any".to_string()
            } else {
                kinds.join(" or ")
            }
        }
        _ => "any".to_string(),
    }
}

/// The strings of a JSON array, or the keys of an object, in document order.
fn string_list(value: &Value) -> Vec<&str> {
    match value {
        Value::Array(items) => items.iter().filter_map(|v| v.as_str()).collect(),
        Value::Object(map) => map.keys().map(String::as_str).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
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

/// How often a long wait checks for somebody trying to say something.
///
/// See `docs/notes/server-tools-mod.md`.
const INBOX_POLL: Duration = Duration::from_millis(250);

/// How often a waiting read asks whether the work it waits for is done.
const WAIT_POLL: Duration = Duration::from_millis(100);

/// How long a call told its wait is over has to answer on its own path.
///
/// # Invariant
///
/// A grace, not a wait: every tool that waits reads its token between polls, so a call
/// that does not answer within a poll or two has nothing to return early with.
const EARLY_RETURN: Duration = Duration::from_secs(1);

/// How a spill file is named: `session-shell-description-callid`, the same for both paths.
fn spill_label(session: &str, shell: &str, description: Option<&str>, call_id: &str) -> String {
    match description {
        Some(description) => format!("{session}-{shell}-{description}-{call_id}"),
        None => format!("{session}-{shell}-{call_id}"),
    }
}

/// The one cap, applied to what a call answered.
///
/// # Invariant
///
/// One cap, one place, one unit. Every answer a call gives passes through here -- a
/// tool's, a command's, a skill's -- so a call cannot be capped twice and the smaller
/// number cannot win. `max_output` states it; the home's `[tools] max_output_chars` is
/// the default; `0` either way is no cap.
///
/// # Invariant
///
/// What is spilled is the WHOLE answer, and the notice names that file. A cap that
/// spilled its own truncated text would point the model at a lie.
pub(crate) fn capped(
    ctx: &ToolContext,
    tool: &str,
    shared: &laws::Given,
    max: usize,
    outcome: ToolOutcome,
) -> ToolOutcome {
    if max == 0 || outcome.output.chars().count() <= max {
        return outcome;
    }
    // An endpoint session's answer is not spilled here: its path is not one the reader
    // can open. It is still cut.
    let dir = (!ctx.on_endpoint(ctx.machine)).then_some(ctx.output_dir.as_path());
    let label = spill_label(
        &ctx.session,
        tool,
        shared.description.as_deref(),
        &ctx.call_id,
    );
    ToolOutcome {
        output: crate::tools::spill::cap_output(
            crate::prompts::shared(),
            &outcome.output,
            max,
            dir,
            &label,
        ),
        ok: outcome.ok,
    }
}

/// Runs one tool and records a failure where the disk monitor can see it.
///
/// # Invariant
///
/// One place runs a tool, so a call the tunnel detached is recorded like a call it
/// waited for: the same failure note, from the same code.
pub(crate) async fn invoke(
    name: &str,
    tool: Arc<dyn Tool>,
    args: Value,
    ctx: &ToolContext,
) -> ToolOutcome {
    let outcome = waited(&tool, &args, ctx).await;
    // The machine this call ran on reports its own room, through the one space the
    // session reads and runs in.
    let peer = match &ctx.remote {
        Some(machine) => {
            let space = ctx.space_on(crate::session::Machine::Remote);
            let volumes = match space.is_local() {
                true => Vec::new(),
                false => space
                    .free_space(&ctx.cwd.to_string_lossy())
                    .await
                    .map(|free| {
                        vec![crate::disk::Volume {
                            label: machine.clone(),
                            free: Some(free),
                        }]
                    })
                    .unwrap_or_default(),
            };
            Some(crate::disk::Peer {
                machine: machine.clone(),
                volumes,
            })
        }
        None => None,
    };
    crate::disk::note_tool_end(
        &ctx.store,
        &ctx.session,
        name,
        &ctx.cwd,
        outcome.ok,
        peer,
        ctx.disk_low,
    )
    .await;
    outcome
}

// ----------------------------------- wait -----------------------------------

/// SPEC's `wait`: stay in this turn until something lands for this session.
///
/// # Invariant
///
/// It consumes nothing it waited for: every arrival is delivered at the next step
/// boundary, so a tool result is never a second copy of a message.
/// How long `wait` waits when the call names no `time`.
pub const WAIT_DEFAULT: Duration = Duration::from_secs(120);

pub struct WaitTool {
    /// The ceiling one call waits, when the call names none.
    pub timeout: Duration,
}

impl Cmd for WaitTool {
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            // SPEC: "if none, wait default time, if 0, return immediately".
            let wait = args
                .get("time")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(self.timeout);
            // A cancel that finds this mark keeps the turn, and resumes this wait.
            let _waiting = ctx.stop.begin_wait(wait);
            let began = std::time::Instant::now();
            let deadline = began + wait;
            loop {
                // What arrived is delivered at the next step boundary, so the answer
                // is the time this call spent.
                match ctx.store.pending_injects_of(&ctx.session) {
                    Ok(rows) if !rows.is_empty() => return ToolOutcome::ok(spent(began)),
                    Ok(_) => {}
                    // An unreadable inbox is said, not waited out: the retry would
                    // see the same failure for the whole ceiling.
                    Err(e) => return ToolOutcome::err(format!("could not read the inbox: {e}")),
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return ToolOutcome::ok(spent(began));
                }
                tokio::select! {
                    _ = tokio::time::sleep(remaining.min(WAIT_POLL)) => {}
                    // SPEC: a waiting call returns early -- with the time it spent.
                    _ = ctx.cancel.cancelled() => return ToolOutcome::ok(spent(began)),
                }
            }
        })
    }
}

/// SPEC's `time` for this call: how long it waited, spelled as a `Result`'s is.
fn spent(began: std::time::Instant) -> String {
    format!("{:.3}s", began.elapsed().as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape `exec` has: nothing is required, and its words are `args`.
    fn exec_schema() -> Value {
        json!({
            "properties": {
                "bin": {"type": "string"},
                "args": {"type": "array"},
                "id": {"type": "string"},
            }
        })
    }

    #[test]
    fn a_key_the_tool_does_not_take_is_named_even_when_nothing_is_missing() {
        let why = argument_error_for(
            "exec",
            &exec_schema(),
            &json!({"command": ["bash", "-lc", "echo hi"]}),
        )
        .expect("a call naming an argument the tool does not take is refused");
        assert!(
            why.contains("`command` is not an argument it takes"),
            "{why}"
        );
        assert!(why.contains("`exec` takes:"), "{why}");
    }

    #[test]
    fn the_arguments_the_schema_declares_answer_with_nothing() {
        assert_eq!(
            argument_error_for("exec", &exec_schema(), &json!({"bin": "bash"})),
            None
        );
    }

    /// A schema is handed the shared arguments by the call layer, so a row that
    /// declares none still offers them.
    #[test]
    fn a_schema_that_declares_nothing_is_still_offered_the_shared_arguments() {
        let offered = laws::schema(json!({"properties": {}}));
        for (name, kind, _) in laws::ALL {
            assert_eq!(offered["properties"][name]["type"], *kind, "{name}");
        }
    }

    /// A declared property wins: the row's own kind and sentence are not overwritten.
    #[test]
    fn a_property_the_row_declares_keeps_what_it_said() {
        let offered = laws::schema(json!({
            "properties": {"description": {"type": "integer"}}
        }));
        assert_eq!(offered["properties"]["description"]["type"], "integer");
    }

    /// The check must not refuse what the schema offers: a shared argument is not one
    /// "it does not take".
    #[test]
    fn a_shared_argument_is_never_an_argument_the_tool_does_not_take() {
        let offered = laws::schema(exec_schema());
        for (name, _, _) in laws::ALL {
            let given = if *name == "max_output" {
                json!(8000)
            } else {
                json!("check the state")
            };
            let mut args = serde_json::Map::new();
            args.insert("bin".to_string(), json!("bash"));
            args.insert((*name).to_string(), given);
            assert_eq!(
                argument_error_for("exec", &offered, &Value::Object(args)),
                None,
                "{name}"
            );
        }
    }

    /// Taken off the arguments: a command would answer for a name its handler never
    /// declared.
    #[test]
    fn the_shared_arguments_are_taken_off_and_read() {
        let mut args = json!({
            "description": "  check the state  ",
            "max_output": 8000,
            "path": "a-file",
        });
        let given = laws::Given::take(&mut args);
        assert_eq!(given.description.as_deref(), Some("check the state"));
        assert_eq!(given.max_output, Some(8000));
        assert_eq!(args, json!({ "path": "a-file" }));
    }

    /// One cap: a call's number, or the home's default, and never the smaller of two.
    ///
    /// # Invariant
    ///
    /// The defect this pins: a tool that capped as well, so a `max_output` above the
    /// home's default was silently lowered to it, and asking for more returned less.
    #[test]
    fn the_call_s_number_raises_the_cap_rather_than_losing_to_the_default() {
        let mut args = json!({ "max_output": 50_000 });
        let given = laws::Given::take(&mut args);
        assert_eq!(given.cap(8_000), 50_000);
        // And a call that states none is held to the home's default.
        assert_eq!(laws::Given::default().cap(8_000), 8_000);
    }

    /// A count written as text is still a count: a provider may hand one back as a string.
    #[test]
    fn a_max_output_written_as_text_is_read_as_a_number() {
        let mut args = json!({ "max_output": "12000" });
        assert_eq!(laws::Given::take(&mut args).max_output, Some(12_000));
    }

    /// The cap counts characters, so a stated length means the same in every language.
    ///
    /// # Invariant
    ///
    /// The defect this pins: the cap counted bytes, so 4000 CJK characters (12000 bytes)
    /// were cut to a third of themselves and the notice reported a byte count as though
    /// it were a length.
    #[test]
    fn the_cap_counts_characters_not_bytes() {
        let text = "\u{4e2d}".repeat(4_000);
        assert!(
            text.len() > 8_000,
            "the text has to be over the cap in bytes"
        );
        let kept =
            crate::tools::spill::cap_output(crate::prompts::shared(), &text, 8_000, None, "t");
        // 4000 characters is under 8000 characters: nothing is taken away at all.
        assert_eq!(kept.chars().count(), 4_000, "the whole text survives");
        assert_eq!(kept, text, "not one character is cut");
        // The answer keeps the stated length, head and tail, counted rather than read
        // from the notice's prose.
        let cut =
            crate::tools::spill::cap_output(crate::prompts::shared(), &text, 1_000, None, "t");
        let heads = cut.chars().take(600).filter(|c| *c == '\u{4e2d}').count();
        let tails = cut
            .chars()
            .rev()
            .take(400)
            .filter(|c| *c == '\u{4e2d}')
            .count();
        assert_eq!(
            heads, 600,
            "the head is three fifths of the cap, in characters"
        );
        assert_eq!(tails, 400, "the tail is the rest");
        assert!(
            cut.chars().count() >= 1_000,
            "the text kept is the cap itself; a notice, where a home supplies one, rides on top"
        );
    }

    #[test]
    fn a_missing_required_argument_still_says_which() {
        let schema = json!({
            "required": ["path"],
            "properties": {"path": {"type": "string"}},
        });
        let why =
            argument_error_for("read_file", &schema, &json!({})).expect("an empty call is refused");
        assert!(why.contains("`path` is required"), "{why}");
    }
}
