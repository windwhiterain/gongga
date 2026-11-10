//! Tools the model calls: persistent shells and the jobs they leave behind.
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
pub mod file;
#[cfg(windows)]
pub mod job;
pub mod live_compact;
pub mod python_remote;
pub mod skill;
pub mod spec;
pub mod spill;
pub mod tunnel;
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
    /// The jobs running right now, so `del_job` can stop one.
    pub jobs: tunnel::JobRegistry,
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
    /// Whether this call is holding a turn, which is what gives its clock a meaning.
    ///
    /// # Invariant
    ///
    /// False for a job: a call that already answers without waiting holds no turn, so
    /// nothing is left for `timeout_ms` to take away from it.
    pub holds_the_turn: bool,
    /// Whether a `cancel` is ending this turn, said where a tool call can hear it.
    pub stop: crate::session::StopSignal,
    /// Set when shells end because the server is going: nothing that stops then finished.
    pub closing: Arc<std::sync::atomic::AtomicBool>,
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

    /// The connection this session's references are read through, if any.
    pub(crate) fn reader(&self) -> Option<Arc<crate::remote::Reader>> {
        self.reader.clone()
    }

    /// Where `machine` starts: the session's directory, empty on the other machine.
    pub(crate) fn dir(&self, machine: Machine) -> PathBuf {
        self.workspace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dir(machine)
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
    fn spec(&self) -> &spec::ToolSpec;

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

    /// How long this tool's own calls wait when the call states none.
    ///
    /// # Invariant
    ///
    /// A default, never a ceiling: the tunnel resolves the wait from this and its own
    /// cap, and writes the answer into the call. See `tunnel::resolve`.
    fn default_timeout(&self) -> Option<Duration>;

    /// Whether this tool is subject to the tunnel's shared arguments.
    ///
    /// # Invariant
    ///
    /// `wait` is the one tool that answers no: SPEC treats it internally, because
    /// waiting is its whole work -- a ceiling on it would keep it from waiting for what
    /// it was asked to wait for, and `background` would be a way of not waiting at all.
    fn tunneled(&self) -> bool;

    /// What carries a call to this tool that says `background`.
    ///
    /// # Invariant
    ///
    /// `Here` is the answer for a tool that acts on this server's state: a process of its
    /// own would find none of it, so `background` is refused rather than answered with a
    /// job that would die with the server. See `docs/notes/server-tools-worker.md`.
    fn detached_carrier(&self) -> tunnel::Carried;

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

    /// Whether `background` is this tool's own work rather than the tunnel's.
    ///
    /// # Invariant
    ///
    /// A tool that backgrounds into a shell or an interpreter is read back by that
    /// collector (`check_bash`, `check_python`); every other tool's background is a
    /// process of its own, read by `check_job`. See `docs/notes/server-tools-mod.md`.
    fn handles_background(&self) -> bool;

    /// Whether this tool's wait is its own, so the tunnel's clock does not end it.
    ///
    /// # Invariant
    ///
    /// A shell or an interpreter turns a long wait into its own background job, and a
    /// reading tool waits for `wait_ms`: in both the work *is* the wait, so a clock
    /// taking the call away would be taking away the thing that was asked for.
    fn owns_its_wait(&self) -> bool;

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
    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(layer) = crate::commands::layer() else {
                return ToolOutcome::err(format!(
                    "{} is a command, and this process has no command layer: it runs one \
                     call and answers with a file",
                    self.name
                ));
            };
            let words = match args.as_object() {
                Some(table) => named_words(table),
                None => protocol::Args::new(),
            };
            let began = std::time::Instant::now();
            // SPEC: a waiting call returns early; a command that said nothing has nothing.
            let outcome = tokio::select! {
                outcome = layer.run(&ctx.session, &self.name, words) => Some(outcome),
                _ = ctx.cancel.cancelled() => None,
            };
            let ended = match &outcome {
                Some(done) => json!({
                    "id": Value::Null,
                    "output": done.message,
                    "time": format!("{:.3}s", began.elapsed().as_secs_f64()),
                    "matched": false,
                    "exit": if done.ok { 0 } else { 1 },
                    "data": done.data,
                }),
                None => json!({
                    "id": Value::Null,
                    "output": "",
                    "time": format!("{:.3}s", began.elapsed().as_secs_f64()),
                    "matched": false,
                    "exit": Value::Null,
                }),
            };
            // Ended early is not a failure: the command said nothing, because it was not
            // given the time to.
            match outcome {
                Some(done) if !done.ok => ToolOutcome::err(ended.to_string()),
                _ => ToolOutcome::ok(ended.to_string()),
            }
        })
    }
}

/// A tool call's arguments as the words a command reads.
///
/// # Invariant
///
/// A string is itself and anything else is its JSON, because a command's arguments are
/// words: `cmd` is the one key this drops, since it names the command rather than one of
/// its arguments. See `docs/notes/server-tools-mod.md`.
fn named_words(table: &serde_json::Map<String, Value>) -> protocol::Args {
    let mut words = protocol::Args::new();
    for (key, value) in table {
        if key == "cmd" {
            continue;
        }
        let text = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        words.insert(key.clone(), text);
    }
    words
}

/// One row of the config, offered as the tool the model and the operator call.
pub struct Specd {
    /// The row's name: the key it is declared under.
    name: String,
    /// The row: everything said about this tool.
    spec: spec::ToolSpec,
    /// The command the row names, which does the work.
    work: Box<dyn Cmd>,
}

impl Specd {
    pub fn new(name: String, spec: spec::ToolSpec, work: Box<dyn Cmd>) -> Self {
        Self { name, spec, work }
    }
}

impl Tool for Specd {
    fn name(&self) -> &str {
        &self.name
    }

    fn spec(&self) -> &spec::ToolSpec {
        &self.spec
    }

    fn machine(&self, args: &Value, ctx: &ToolContext) -> Machine {
        self.work
            .machine(args, ctx)
            .unwrap_or_else(|| self.spec.machine.machine())
    }

    fn offered(&self) -> bool {
        self.spec.offered
    }

    fn is_remote(&self) -> bool {
        self.spec.machine == spec::Side::Remote
    }

    fn default_timeout(&self) -> Option<Duration> {
        self.spec.timeout_ms.map(Duration::from_millis)
    }

    fn tunneled(&self) -> bool {
        self.spec.tunneled
    }

    fn detached_carrier(&self) -> tunnel::Carried {
        self.spec.carrier.carried()
    }

    fn writes(&self) -> bool {
        self.spec.writes
    }

    fn handles_background(&self) -> bool {
        self.spec.handles_background
    }

    fn owns_its_wait(&self) -> bool {
        self.spec.owns_its_wait
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        self.work.run(args, ctx)
    }
}

/// A row with the tunnel's own arguments added, worded once for every tool that
/// takes them.
///
/// # Invariant
///
/// A row that declares one of them keeps its own: these are what the tunnel offers,
/// and a tool that words `background` for itself is saying what the shared sentence
/// does not.
fn with_tunnel_arguments(
    row: &spec::ToolSpec,
    tunnel: &std::collections::BTreeMap<String, String>,
) -> spec::ToolSpec {
    let mut row = row.clone();
    if !row.tunneled {
        return row;
    }
    for name in GENERAL_ARGS {
        if row.parameters.iter().any(|argument| argument.name == *name) {
            continue;
        }
        let Some(sentence) = tunnel.get(*name) else {
            continue;
        };
        // `max_output` and `timeout_ms` are counts; the other two are not.
        let kind = match *name {
            "max_output" | "timeout_ms" => "integer",
            "background" => "boolean",
            _ => "string",
        };
        row.parameters.push(spec::Argument {
            name: (*name).to_string(),
            kind: kind.to_string(),
            items: None,
            required: false,
            positional: None,
            hint: None,
            completion: spec::Completion::None,
            sentence: Some(sentence.clone()),
        });
    }
    row
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    /// The longest one call may wait for its answer. See `tunnel::resolve`.
    max_wait: Duration,
    /// What a command needs that no config row states.
    settings: Settings,
}

/// What this build hands a command the config names.
///
/// # Invariant
///
/// Not a tool and not a row: the numbers and the programs a command is built with,
/// which belong to the machine and not to what the model is told.
struct Settings {
    bash_timeout: Duration,
    max_output_chars: usize,
    python: Option<(crate::config::PythonConfig, Option<std::path::PathBuf>)>,
    skill: Option<SkillSettings>,
    live_compact: bool,
}

struct SkillSettings {
    programs: ::skill::Programs,
    lib: Option<std::path::PathBuf>,
    timeout: Duration,
    max_output_chars: usize,
}

impl ToolRegistry {
    /// A registry with no tools yet: the rows come from the config, in [`Self::build`].
    ///
    /// # Invariant
    ///
    /// Nothing is registered here. What a build carries is what the home's `[tools]`
    /// declares, so a row added in the config is a tool and a row removed is not.
    pub fn settings(max_wait: Duration, bash_timeout: Duration, max_output_chars: usize) -> Self {
        Self {
            tools: Vec::new(),
            max_wait,
            settings: Settings {
                bash_timeout,
                max_output_chars,
                python: None,
                skill: None,
                live_compact: false,
            },
        }
    }

    /// Whether this build carries the compaction checkpoint's tool at all.
    pub fn with_live_compact(mut self, enabled: bool) -> Self {
        self.settings.live_compact = enabled;
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

    /// The entries a `skill` row may start, and where this home keeps its import.
    pub fn with_skills(
        mut self,
        programs: ::skill::Programs,
        lib: Option<std::path::PathBuf>,
        timeout: Duration,
        max: usize,
    ) -> Self {
        self.settings.skill = Some(SkillSettings {
            programs,
            lib,
            timeout,
            max_output_chars: max,
        });
        self
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
    pub fn build(
        mut self,
        rows: &std::collections::BTreeMap<String, spec::ToolSpec>,
        tunnel: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        for (name, row) in rows {
            if row.cmd == live_compact::LIVE_COMPACT_TOOL && !self.settings.live_compact {
                continue;
            }
            // The tunnel's own arguments, in the words it words them once.
            let row = &with_tunnel_arguments(row, tunnel);
            match self.command(row) {
                Ok(work) => self
                    .tools
                    .push(Arc::new(Specd::new(name.clone(), row.clone(), work))),
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
        let max_output_chars = row
            .max_output_chars
            .unwrap_or(self.settings.max_output_chars);
        let bash_timeout = row
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(self.settings.bash_timeout);
        Ok(match row.cmd.as_str() {
            "wait" => Box::new(WaitTool {
                timeout: bash_timeout,
            }),
            // The environment's own Python, once per call: no interpreter is kept.
            "read_file" | "write_file" | "replace" => {
                let (python, _lib) = self
                    .settings
                    .python
                    .as_ref()
                    .ok_or("this build configures no Python")?;
                let cap = row.max_output_chars.unwrap_or(python.max_output_chars);
                match row.cmd.as_str() {
                    "read_file" => Box::new(file::ReadFileTool {
                        program: python.program.clone(),
                        args: python.args.clone(),
                        timeout: python.timeout,
                        max_output_chars: cap,
                    }),
                    "write_file" => Box::new(file::WriteFileTool {
                        program: python.program.clone(),
                        args: python.args.clone(),
                        timeout: python.timeout,
                        max_output_chars: cap,
                    }),
                    _ => Box::new(file::ReplaceTool {
                        program: python.program.clone(),
                        args: python.args.clone(),
                        timeout: python.timeout,
                        max_output_chars: cap,
                    }),
                }
            }
            "skill" => {
                let skill = self
                    .settings
                    .skill
                    .as_ref()
                    .ok_or("this build configures no skill programs")?;
                Box::new(skill::SkillTool {
                    programs: skill.programs.clone(),
                    lib: skill.lib.clone(),
                    timeout: skill.timeout,
                    max_output_chars: max_output_chars.min(skill.max_output_chars),
                })
            }
            "exec" => {
                // The channels are the call's own, and the default wait is the row's.
                Box::new(exec::ExecTool {
                    wait: exec::Wait {
                        default: row
                            .timeout_ms
                            .map(Duration::from_millis)
                            .unwrap_or(bash_timeout),
                        max_output_chars,
                    },
                })
            }
            "live_compact" => Box::new(live_compact::LiveCompactTool),
            // SPEC's `fn <tool>(**args)`: a row an operator reaches by name may be a
            // command this build has no type for. One the model is offered may not --
            // a tool nobody implements is not a tool SPEC has. See `server-tools-mod`.
            other if row.offered => {
                return Err(format!(
                    "`{other}` is offered to the model, and this build implements no such tool"
                ));
            }
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
    /// while a request is built. See `docs/notes/server-tools.md`.
    ///
    /// A tool that drives a machine this session does not have is not in the set at
    /// all, and neither is an argument that names one: a model can call what it is
    /// offered, so what it is offered has to be able to answer.
    pub fn stamp(
        &self,
        has_remote: bool,
        with_live_compact: bool,
    ) -> Vec<crate::llm::ToolDefinition> {
        let mut definitions: Vec<crate::llm::ToolDefinition> = self
            .tools
            .iter()
            .filter(|t| t.offered())
            .filter(|t| t.name() != live_compact::LIVE_COMPACT_TOOL)
            .filter(|t| has_remote || !t.is_remote())
            .map(|t| Self::definition_of(t, has_remote))
            .collect();
        // Once a lineage has a compaction origin the schema stays for its life, so
        // the head stops moving twice per cycle.
        if with_live_compact && let Some(tool) = self.get(live_compact::LIVE_COMPACT_TOOL) {
            definitions.push(Self::definition_of(&tool, has_remote));
        }
        definitions
    }

    fn definition_of(tool: &Arc<dyn Tool>, has_remote: bool) -> crate::llm::ToolDefinition {
        let mut parameters = general_parameters(tool.as_ref());
        if !has_remote
            && let Some(properties) = parameters
                .get_mut("properties")
                .and_then(Value::as_object_mut)
        {
            properties.remove("remote");
        }
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
    /// Every tool call goes through here -- a model's call, a script's, a command
    /// layer's -- so this is the one place a failing tool is noticed. What it
    /// notices is not the tool's own words (`check_bash` sees a field, not a
    /// sentence) but the room left where the harness writes: see
    /// `docs/notes/server-disk.md`.
    pub async fn run(
        self: &Arc<Self>,
        name: &str,
        mut args: Value,
        ctx: &ToolContext,
    ) -> ToolOutcome {
        let Some(tool) = self.get(name) else {
            return ToolOutcome::err(format!(
                "unknown tool {name:?}; available: {}",
                self.names().join(", ")
            ));
        };
        // The name this call was made under, as the row spells it now.
        let mut named = ctx.clone();
        named.tool = name.to_string();
        let ctx = &named;
        // Before the arguments are read: a refused call must not reach the tool at all,
        // so nothing is spawned for it.
        if tool.writes() && ctx.explore.is_some() {
            return ToolOutcome::err(crate::explore::refusal(name));
        }
        if let Some(message) = argument_error(tool.as_ref(), &args) {
            return ToolOutcome::err(message);
        }
        // After the schema check, so the number written back is not a word the call was
        // never allowed to say.
        tunnel::resolve(tool.as_ref(), &mut args, self.max_wait);
        // The tunnel's words are the tunnel's to offer: `wait` takes none of them.
        if let Some(word) = tunnel::foreign(tool.as_ref(), &args) {
            return ToolOutcome::err(format!(
                "`{name}` did not run: it said `{word}`, and `{name}` is the one tool the tunnel \
                 leaves alone -- waiting is its whole work, so it has no answer to leave running. \
                 Call it without `{word}`."
            ));
        }
        // SPEC: a background call is a process of its own, so it outlives a cancel.
        if tunnel::wants_background(&args) {
            match tool.detached_carrier() {
                tunnel::Carried::Process => {}
                tunnel::Carried::Worker => {
                    return tunnel::spawn_worker(tool, name.to_string(), args, ctx).await;
                }
                tunnel::Carried::Here if !tool.handles_background() => {
                    return ToolOutcome::err(format!(
                        "`{name}` cannot run in the background: a background call is a process \
                         of its own, and `{name}` acts on what this server holds -- its shells, \
                         its interpreters, its jobs. Call it in the foreground."
                    ));
                }
                tunnel::Carried::Here => {}
            }
        }
        let outcome = invoke(name, tool, args, ctx).await;
        // The call ended; the look is a task of its own, so nothing waits on it.
        crate::explore::after_job(ctx, name);
        outcome
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}

/// Waits for one call, and ends that wait when somebody speaks or the turn stops.
///
/// # Invariant
///
/// The only wait, so "a message reaches the turn" is one rule and not one per tool.
/// The work runs on a task with a token of its own: that is what lets the clock take
/// the turn away without stopping it. See `docs/notes/server-tools-mod.md`.
async fn waited(tool: &Arc<dyn Tool>, args: &Value, ctx: &ToolContext) -> ToolOutcome {
    let started = std::time::Instant::now();
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let max = args
        .get("max_output")
        .and_then(Value::as_u64)
        .map(|max| usize::try_from(max).unwrap_or(usize::MAX))
        .unwrap_or(crate::config::DEFAULT_MAX_OUTPUT_CHARS);
    // This call's own clock: a job holds no turn, and a tool whose wait is its work
    // is not on the clock at all.
    let clock_at = (ctx.holds_the_turn && tool.tunneled() && !tool.owns_its_wait())
        .then(|| tunnel::wait_of(args))
        .flatten()
        .map(|wait| tokio::time::Instant::now() + wait);
    let mut job = ctx.clone();
    job.cancel = CancellationToken::new();
    let token = job.cancel.clone();
    let work = Arc::clone(tool);
    let mut given = args.clone();
    // A tool whose wait is the tunnel's gets no deadline of its own: the clock is
    // the turn's, and the work outlives it.
    if !tool.owns_its_wait()
        && let Some(object) = given.as_object_mut()
    {
        object.remove("timeout_ms");
    }
    let (tx, mut rx) = mpsc::channel::<ToolOutcome>(1);
    let task = tokio::spawn(async move {
        let outcome = work.run(given, &job).await;
        let _ = tx.send(outcome).await;
    });
    let mut punctual = tokio::time::interval(INBOX_POLL);
    let why = loop {
        tokio::select! {
            answer = rx.recv() => return answer.unwrap_or_else(|| stopped(tool.name())),
            _ = ctx.cancel.cancelled() => break Some(match ctx.stop.interrupted() {
                true => "interrupt",
                false => "cancel",
            }),
            _ = punctual.tick() => {
                if ctx.inbox.arrived() {
                    break Some("message");
                }
            }
            _ = wait_until(clock_at) => break None,
        }
    };
    let Some(why) = why else {
        // Out of time, not out of work: the turn stops waiting, the job does not.
        return promote(
            ctx,
            tool.name(),
            description.as_deref(),
            max,
            rx,
            token,
            started,
        )
        .await;
    };
    tracing::info!(
        session = %ctx.session,
        tool = %tool.name(),
        why,
        elapsed = ?started.elapsed(),
        "the wait ended before the call answered"
    );
    // SPEC: a waiting call returns early; its token says the wait is over. A tool
    // with no wait to end is stopped instead.
    token.cancel();
    match tokio::time::timeout(EARLY_RETURN, rx.recv()).await {
        Ok(Some(answer)) => answer,
        _ => {
            task.abort();
            let _ = task.await;
            stopped(tool.name())
        }
    }
}

/// Sleeps until `at`, or for ever when this call has no clock of its own.
async fn wait_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

/// What a call says when its own work stopped without answering.
fn stopped(name: &str) -> ToolOutcome {
    ToolOutcome::err(format!(
        "`{name}` ended without answering: the work it was running stopped before it could \
         say how it went."
    ))
}

/// Hands a call that outlived its clock to the job table, and answers for it.
///
/// # Invariant
///
/// The id is in the receipt before the caller can ask for it, and one forwarder owns
/// the answer: what arrives later is written where `check_job` reads it, in the same
/// act that tells the session. Nothing else holds that text.
async fn promote(
    ctx: &ToolContext,
    tool: &str,
    description: Option<&str>,
    max: usize,
    mut rx: mpsc::Receiver<ToolOutcome>,
    token: CancellationToken,
    started: std::time::Instant,
) -> ToolOutcome {
    // The row starts when the work did, not when the clock gave up on it: one job
    // must not read as two different ages.
    let began =
        crate::store::now_millis() - i64::try_from(started.elapsed().as_millis()).unwrap_or(0);
    let id = match ctx.store.add_job(
        &ctx.session,
        &ctx.call_id,
        tool,
        description,
        tunnel::Mode::Background.as_str(),
        began,
    ) {
        Ok(id) => id,
        Err(e) => {
            return ToolOutcome::err(format!(
                "`{tool}` is still running after {} and this call could not be recorded as a \
                 job ({e}), so nothing would collect what it answers.",
                crate::agent::spent(started.elapsed().as_millis() as u64)
            ));
        }
    };
    let waited = crate::agent::spent(started.elapsed().as_millis() as u64);
    let mut sink = ctx.clone();
    // The stream belonged to the turn that is no longer waiting.
    sink.events = None;
    let named = tool.to_string();
    let described = description.map(str::to_string);
    // Registered before the receipt leaves, so `del_job` reaches it from the first
    // instant the caller could name it.
    ctx.jobs.add(id, token);
    tokio::spawn(async move {
        let Some(outcome) = rx.recv().await else {
            return;
        };
        sink.jobs.done(id);
        let text = match crate::tools::cap_max_output(
            &sink,
            &named,
            described.as_deref(),
            &sink.call_id,
            &outcome.output,
            max,
        ) {
            Some(metadata) => format!("{metadata}\n(over {max} chars: it is all in that file)"),
            None => outcome.output,
        };
        let ran = crate::agent::spent(started.elapsed().as_millis() as u64);
        tunnel::report(&sink, id, &named, &text, outcome.ok, &ran).await;
    });
    ToolOutcome::ok(format!(
        "`{tool}` was still running after {waited}, so this call became background job {id}. \
         It runs on without this turn: `check_job` with `job_id: {id}` collects what it \
         answers, and this session is told when it ends."
    ))
}

/// The adapter's general parameters: every tool may carry them, none
/// declares them by hand.
///
/// # Invariant
///
/// Injected into the model-facing schema in `definitions` (a declared
/// property wins), read back at the call layer; they never reach the
/// command surface, which reads `Tool::parameters` raw.
pub const GENERAL_ARGS: &[&str] = &["description", "max_output", "timeout_ms", "background"];

/// SPEC's adapter-level parameters added to every tool the model is offered.
///
/// # Invariant
///
/// Injected here, in the model-facing schema only, so a provider's validator
/// accepts what the call layer reads. A property a tool already declares wins:
/// these are the defaults, not an override.
pub(crate) fn general_parameters(tool: &dyn Tool) -> Value {
    let mut schema = tool.spec().schema();
    // SPEC: `description` is every call's; the rest are the tunnel's.
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties
            .entry("description".to_string())
            .or_insert_with(|| {
                json!({
                    "type": "string"
                })
            });
    }
    // The tunnel's other words are the tunnel's: a tool left alone takes none.
    if tool.tunneled()
        && let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut)
    {
        properties
            .entry("max_output".to_string())
            .or_insert_with(|| {
                json!({
                    "type": "integer"
                })
            });
        properties
            .entry("timeout_ms".to_string())
            .or_insert_with(|| {
                json!({
                    "type": "integer"
                })
            });
        properties
            .entry("background".to_string())
            .or_insert_with(|| {
                json!({
                    "type": "boolean"
                })
            });
    }
    schema
}

/// Why a call does not fit the declared schema, in the schema's own words.
///
/// See `docs/notes/server-tools-mod.md`.
fn argument_error(tool: &dyn Tool, args: &Value) -> Option<String> {
    let schema = tool.spec().schema();
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
    let path = crate::tools::spill::spill(&ctx.output_dir, &label, text)?;
    Some(
        json!({
            "path": path,
            "chars": text.chars().count(),
            "line_count": text.lines().count(),
        })
        .to_string(),
    )
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
    // The machine this call ran on reports its own room.
    let peer = match (&ctx.remote, &ctx.reader) {
        (Some(machine), reader) => Some(crate::disk::Peer {
            machine: machine.clone(),
            volumes: match reader {
                Some(reader) => reader
                    .space(&ctx.cwd.to_string_lossy())
                    .await
                    .ok()
                    .flatten()
                    .map(|free| {
                        vec![crate::disk::Volume {
                            label: machine.clone(),
                            free: Some(free),
                        }]
                    })
                    .unwrap_or_default(),
                None => Vec::new(),
            },
        }),
        _ => None,
    };
    crate::disk::note_tool_end(&ctx.store, &ctx.session, name, &ctx.cwd, outcome.ok, peer).await;
    outcome
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
