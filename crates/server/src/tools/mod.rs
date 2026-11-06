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
pub mod exec;
pub mod exec_remote;
pub mod interpreter;
#[cfg(windows)]
pub mod job;
pub mod live_compact;
pub mod python;
pub mod python_remote;
pub mod skill;
pub mod text;
pub mod tunnel;
pub mod words;

/// Everything a tool invocation needs, supplied per call by the agent loop.
///
/// # Invariant
///
/// Copyable, because a call that said `detached` outlives the call that made it: it
/// runs as a job of this server, with its own cancellation and no stream of the
/// turn's to write to.
#[derive(Clone)]
pub struct ToolContext {
    pub session: String,
    pub call_id: String,
    /// The home this call belongs to: what a worker needs to be started.
    pub home: PathBuf,
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

    /// Where one machine's shells run, for a tool that needs another program there.
    ///
    /// # Invariant
    ///
    /// `None` when the session has no shells on that machine, which is what a tool
    /// reports rather than starting the program on the wrong one.
    pub(crate) fn endpoint(&self, machine: Machine) -> Option<crate::remote::endpoint::Endpoint> {
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .endpoint(machine)
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

    /// Whether this tool drives the session's remote.
    ///
    /// # Invariant
    ///
    /// The tool answers for itself, so a twin added tomorrow cannot be offered to a
    /// session that has no such machine. See `definitions`.
    fn is_remote(&self) -> bool {
        false
    }

    /// How long this tool's own calls wait when the call states none.
    ///
    /// # Invariant
    ///
    /// A default, never a ceiling: the tunnel resolves the wait from this and its own
    /// cap, and writes the answer into the call. See `tunnel::resolve`.
    fn default_timeout(&self) -> Option<Duration> {
        None
    }

    /// Whether this tool is subject to the tunnel's shared arguments.
    ///
    /// # Invariant
    ///
    /// `wait` is the one tool that answers no: SPEC treats it internally, because
    /// waiting is its whole work -- a ceiling on it would keep it from waiting for what
    /// it was asked to wait for, and `detached` would be a way of not waiting at all.
    fn tunneled(&self) -> bool {
        true
    }

    /// What carries a call to this tool that says `detached`.
    ///
    /// # Invariant
    ///
    /// `Here` is the answer for a tool that acts on this server's state: a process of its
    /// own would find none of it, so `detached` is refused. See
    /// `docs/notes/server-tools-worker.md`.
    fn detached_carrier(&self) -> tunnel::Carried {
        tunnel::Carried::Here
    }

    /// Whether calling this tool is itself a change to the workspace.
    ///
    /// # Invariant
    ///
    /// Said by the tool that knows, not inferred from its name: a session that is for
    /// a read refuses exactly the tools whose arguments *are* the write, since no
    /// earlier or later look could catch what has not happened yet. A tool that
    /// changes the workspace some other way -- through a program -- is caught by the
    /// look its call takes when it ends. See `docs/notes/server-explore.md`.
    fn writes(&self) -> bool {
        false
    }

    /// Whether `background` is this tool's own work rather than the tunnel's.
    ///
    /// # Invariant
    ///
    /// A tool that backgrounds into a shell or an interpreter is read back by that
    /// collector (`check_bash`, `check_python`); every other tool's background is a
    /// job of this server, read by `check_job`. See `docs/notes/server-tools-mod.md`.
    fn handles_background(&self) -> bool {
        false
    }

    /// Whether this tool's wait is its own, so the tunnel's clock does not end it.
    ///
    /// # Invariant
    ///
    /// A shell or an interpreter turns a long wait into its own background job, and a
    /// reading tool waits for `wait_ms`: in both the work *is* the wait, so a clock
    /// taking the call away would be taking away the thing that was asked for.
    fn owns_its_wait(&self) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;

    /// What this call leaves behind when its wait is ended from outside.
    ///
    /// # Invariant
    ///
    /// The wait itself is `ToolRegistry::run`'s, so a message ends every tool's wait
    /// the same way; only the aftermath is the tool's own, and it is read from the
    /// context -- a shell, an interpreter -- because the future that owned it has
    /// been dropped by the time this is called. The default is the honest answer for
    /// a tool with nothing to leave behind: it stopped where it was.
    fn cut<'a>(
        &'a self,
        _args: &'a Value,
        _ctx: &'a ToolContext,
        why: Cut,
        _started: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            ToolOutcome::err(format!(
                "`{}` ended its wait: {}. What it had already done is not reported, because \
                 the call was ended instead of finishing.",
                self.name(),
                why.because()
            ))
        })
    }
}

/// Why a call's wait ended before the tool answered it.
///
/// See `docs/notes/server-tools-mod.md`.
#[derive(Debug, Clone, Copy)]
pub enum Cut {
    /// Someone is talking to this session, and the turn is owed that message.
    Message,
    /// The turn was cancelled, so nothing it started is owed.
    Cancelled,
}

impl Cut {
    /// Why, in the words a model reads.
    pub fn because(self) -> &'static str {
        match self {
            Cut::Message => "a message arrived for this session",
            Cut::Cancelled => "the turn was cancelled",
        }
    }
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    /// The longest one call may wait for its answer. See `tunnel::resolve`.
    max_wait: Duration,
}

impl ToolRegistry {
    pub fn with_builtins(
        max_wait: Duration,
        bash_timeout: Duration,
        max_output_chars: usize,
    ) -> Self {
        Self {
            max_wait,
            tools: vec![
                Arc::new(BashTool {
                    machine: Machine::Local,
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Arc::new(BashTool {
                    machine: Machine::Remote,
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Arc::new(exec::ExecTool {
                    machine: Machine::Local,
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Arc::new(exec::ExecTool {
                    machine: Machine::Remote,
                    timeout: bash_timeout,
                    max_output_chars,
                }),
                Arc::new(CheckBashTool { max_output_chars }),
                Arc::new(CheckJobTool { max_output_chars }),
                Arc::new(WaitTool {
                    timeout: bash_timeout,
                }),
                Arc::new(DelBashTool),
                Arc::new(DelJobTool),
                Arc::new(ListBashTool),
                Arc::new(python::CheckPythonTool),
                Arc::new(python::DelPythonTool),
                Arc::new(python::ListPythonTool),
            ],
        }
    }

    /// Makes compaction callable without offering its schema in ordinary requests.
    pub fn with_live_compact(mut self, enabled: bool) -> Self {
        if enabled {
            self.tools.push(Arc::new(live_compact::LiveCompactTool));
        }
        self
    }

    /// Adds the Python tools; `lib` is where `gongga_skill.py` sits.
    pub fn with_python(
        mut self,
        python: crate::config::PythonConfig,
        lib: Option<std::path::PathBuf>,
    ) -> Self {
        let files = python.clone();
        // One implementation, two names: here, and on the session's peer.
        for machine in [Machine::Local, Machine::Remote] {
            self.tools.push(Arc::new(python::PythonTool {
                machine,
                program: python.program.clone(),
                args: python.args.clone(),
                timeout: python.timeout,
                max_output_chars: python.max_output_chars,
                lib: lib.clone(),
            }));
        }
        self.tools.push(Arc::new(python::ReadFileTool {
            program: files.program.clone(),
            args: files.args.clone(),
            timeout: files.timeout,
            max_output_chars: files.max_output_chars,
        }));
        self.tools.push(Arc::new(python::WriteFileTool {
            program: files.program.clone(),
            args: files.args.clone(),
            timeout: files.timeout,
            max_output_chars: files.max_output_chars,
        }));
        self.tools.push(Arc::new(python::ReplaceTool {
            program: files.program,
            args: files.args,
            timeout: files.timeout,
            max_output_chars: files.max_output_chars,
        }));
        self
    }

    /// Adds `skill`; `lib` is where this home keeps the Python a package imports.
    pub fn with_skills(
        mut self,
        programs: ::skill::Programs,
        lib: Option<std::path::PathBuf>,
        timeout: Duration,
        max: usize,
    ) -> Self {
        self.tools.push(Arc::new(skill::SkillTool {
            programs,
            lib,
            timeout,
            max_output_chars: max,
        }));
        self
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

    /// Tool schemas in the shape the Responses API expects.
    ///
    /// # Invariant
    ///
    /// A tool that drives a machine this session does not have is not offered at
    /// all, and neither is an argument that names one: a model can call what it is
    /// offered, so what it is offered has to be able to answer. See
    /// `docs/notes/server-tools-mod.md`.
    pub fn definitions(&self, has_remote: bool) -> Vec<crate::llm::ToolDefinition> {
        self.tools
            .iter()
            .filter(|t| t.name() != live_compact::LIVE_COMPACT_TOOL)
            .filter(|t| has_remote || !t.is_remote())
            .map(|t| Self::definition_of(t, has_remote))
            .collect()
    }

    /// One explicitly requested schema, outside the ordinary tool list.
    pub fn definition(&self, name: &str, has_remote: bool) -> Option<crate::llm::ToolDefinition> {
        self.get(name)
            .map(|tool| Self::definition_of(&tool, has_remote))
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
            description: text::shared().description(tool.name()).unwrap_or_default(),
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
        if tunnel::both_ways(&args) {
            return ToolOutcome::err(
                "pick one: `background` collects the answer with `check_job`, `detached` hands it \
                 to you as a notice",
            );
        }
        // A tool whose background is its own shell's keeps it; every other one is a job.
        if tunnel::wants_background(&args) && !tool.handles_background() {
            return tunnel::spawn(tool, name.to_string(), args, ctx, tunnel::Mode::Background)
                .await;
        }
        // SPEC: `detached` means a process of its own, so only work that is one can.
        if tunnel::wants_detached(&args) {
            match tool.detached_carrier() {
                tunnel::Carried::Process => {}
                tunnel::Carried::Worker => {
                    return tunnel::detach_to_worker(tool, name.to_string(), args, ctx).await;
                }
                tunnel::Carried::Here => {
                    return ToolOutcome::err(format!(
                        "`{name}` cannot run detached: a detached call is a process of its own, \
                         and `{name}` acts on what this server holds -- its shells, its \
                         interpreters, its jobs. Run it here, or say `background` and collect \
                         it with `check_job`."
                    ));
                }
            }
        }
        let outcome = invoke(name, tool, args, ctx).await;
        // The call ended; the look is a task of its own, so nothing waits on it.
        crate::explore::after_job(ctx, name);
        outcome
    }

    pub fn names(&self) -> Vec<&'static str> {
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
            _ = ctx.cancel.cancelled() => break Some(Cut::Cancelled),
            _ = punctual.tick() => {
                if ctx.inbox.arrived() {
                    break Some(Cut::Message);
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
    // A cut ends the work too, and it is ended before the tool says what it left.
    task.abort();
    let _ = task.await;
    tracing::info!(
        session = %ctx.session,
        tool = %tool.name(),
        why = why.because(),
        elapsed = ?started.elapsed(),
        "the wait ended before the call answered"
    );
    tool.as_ref().cut(args, ctx, why, started).await
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
        tunnel::report(
            &sink,
            id,
            &named,
            &text,
            outcome.ok,
            &ran,
            tunnel::Mode::Background,
        )
        .await;
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
pub const GENERAL_ARGS: &[&str] = &[
    "description",
    "max_output",
    "timeout_ms",
    "background",
    "detached",
];

/// SPEC's adapter-level parameters added to every tool the model is offered.
///
/// # Invariant
///
/// Injected here, in the model-facing schema only, so a provider's validator
/// accepts what the call layer reads. A property a tool already declares wins:
/// these are the defaults, not an override.
pub(crate) fn general_parameters(tool: &dyn Tool) -> Value {
    let mut schema = tool.parameters();
    // A tool the tunnel leaves alone takes none of the tunnel's words. See SPEC.
    if tool.tunneled()
        && let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut)
    {
        properties
            .entry("description".to_string())
            .or_insert_with(|| {
                json!({
                    "type": "string"
                })
            });
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
        properties.entry("detached".to_string()).or_insert_with(|| {
            json!({
                "type": "boolean"
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
    // Last: a sentence is written into a property that must already exist.
    text::shared().apply(tool.name(), &mut schema);
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

/// How often a long wait checks for somebody trying to say something.
///
/// See `docs/notes/server-tools-mod.md`.
const INBOX_POLL: Duration = Duration::from_millis(250);

// ---------------------------------- bash ----------------------------------

/// The command a call asks for, as the line the shell will read.
fn command_line(value: Option<&Value>) -> Result<String, String> {
    match value {
        Some(Value::String(text)) => {
            if text.trim().is_empty() {
                return Err("bash received an empty command".to_string());
            }
            Ok(text.clone())
        }
        Some(other) => Err(format!(
            "bash requires a `command` that is a string, and this one is {kind}. A program \
             and its arguments belong to `exec`, where the words are not read by a shell",
            kind = json_type(other),
        )),
        None => Err("bash requires a `command`".to_string()),
    }
}

/// The name the tool on the session's remote is called by.
const REMOTE_BASH: &str = "remote_bash";

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

    fn machine(&self, _args: &Value, _ctx: &ToolContext) -> Machine {
        self.machine
    }

    fn is_remote(&self) -> bool {
        self.machine == Machine::Remote
    }

    /// A detached `bash` is a process of this machine's registry, not a job of the server.
    fn detached_carrier(&self) -> tunnel::Carried {
        match self.machine {
            // A peer's detached bash would have to run on the peer: refused here.
            Machine::Local => tunnel::Carried::Process,
            Machine::Remote => tunnel::Carried::Here,
        }
    }

    /// Both machines' `background` is a job of that shell, read by `check_bash`.
    fn handles_background(&self) -> bool {
        true
    }

    /// A shell turns a long wait into its own background job: the clock is its own.
    fn owns_its_wait(&self) -> bool {
        true
    }

    fn default_timeout(&self) -> Option<Duration> {
        Some(self.timeout)
    }

    fn parameters(&self) -> Value {
        let mut properties = json!({
            "name": {"type": "string"},
            "command": {"type": "string"},
            "description": {"type": "string"},
            "timeout_ms": {"type": "integer"}
        });
        // A job of a shell is that shell's on either machine; only `bash` declares one.
        properties["background"] = json!({
            "type": "boolean"
        });
        match self.machine {
            Machine::Local => {
                properties["detached"] = json!({
                    "type": "boolean"
                });
            }
            Machine::Remote => {
                properties["target"] = json!({
                    "type": "string"
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

            // Relay output as it arrives; the wait around it is `waited`'s.
            let (tx, mut rx) = mpsc::channel::<BashChunk>(256);
            let mut shell = handle.lock().await;
            // Scoped so the future borrowing `shell` drops before the guard does.
            let started = std::time::Instant::now();
            let outcome = {
                let execute = shell.run(&command, timeout, Some(&tx));
                tokio::pin!(execute);
                loop {
                    tokio::select! {
                        res = &mut execute => break res,
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
                // Its deadline passed: nothing was stopped, the shell still holds it.
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

    /// A command in a shell outlives the wait: it is handed to the background, and
    /// a fresh shell takes its name.
    ///
    /// # Invariant
    ///
    /// Runs after the call's own future is gone: the task it spawns takes this
    /// shell's lock. See `docs/notes/server-tools-mod.md`.
    fn cut<'a>(
        &'a self,
        args: &'a Value,
        ctx: &'a ToolContext,
        why: Cut,
        started: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let name = shell_name(args);
            // Only ever a title: the spill file a cut writes is named the same way.
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some((machine, handle)) = ctx.find(name) else {
                return ToolOutcome::err(format!(
                    "the wait for shell `{name}` ended ({}), and the shell is already gone: \
                     what the command printed after this call is lost with it",
                    why.because()
                ));
            };
            match why {
                Cut::Message => {
                    // Already streamed to the window: the notice carries the rest.
                    let _ = handle.lock().await.take_partial_output();
                    leave_running(
                        ctx,
                        machine,
                        name,
                        &handle,
                        description,
                        self.max_output_chars,
                        started,
                        why.because(),
                    )
                    .await
                }
                Cut::Cancelled => {
                    // Stopped, not abandoned: leftovers would answer the next command.
                    handle.lock().await.kill().await;
                    let replaced = ctx.replace_shell(machine, name, &handle);
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
            }
        })
    }
}

// ------------------------------- check_bash --------------------------------

pub struct CheckBashTool {
    pub max_output_chars: usize,
}

/// How often a waiting read asks whether the work it waits for is done.
const WAIT_POLL: Duration = Duration::from_millis(100);
/// A wait is a tool call, not a turn: beyond this it is better to come back later.
const MAX_WAIT: Duration = Duration::from_secs(600);

impl Tool for CheckBashTool {
    fn name(&self) -> &'static str {
        "check_bash"
    }

    /// The read waits for `wait_ms`; that wait is the whole work.
    fn owns_its_wait(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "wait_ms": {"type": "integer"}
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
                    // Only a clock: a message or a cancel ends this from `waited`.
                    tokio::time::sleep(remaining.min(WAIT_POLL)).await;
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

    /// A read never stopped anything: its jobs are still there to read.
    fn cut<'a>(
        &'a self,
        args: &'a Value,
        _ctx: &'a ToolContext,
        why: Cut,
        _started: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let name = shell_name(args);
            ToolOutcome::err(format!(
                "check_bash stopped waiting for {name}: {}. The jobs it was waiting for were \
                 not stopped, and `check_bash` reads what they printed.",
                why.because()
            ))
        })
    }
}

// -------------------------------- check_job --------------------------------

/// SPEC's `check_job`: what became of a call that did not wait for its answer.
pub struct CheckJobTool {
    pub max_output_chars: usize,
}

impl Tool for CheckJobTool {
    fn name(&self) -> &'static str {
        "check_job"
    }

    /// The read waits for `wait_ms`; that wait is the whole work.
    fn owns_its_wait(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {"type": "integer"},
                "wait_ms": {"type": "integer"}
            },
            "required": ["job_id"],
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(id) = args
                .get("job_id")
                .and_then(Value::as_u64)
                .and_then(|id| i64::try_from(id).ok())
            else {
                return ToolOutcome::err("check_job needs the `job_id` its start answered with");
            };
            let wait = args
                .get("wait_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(Duration::ZERO)
                .min(MAX_WAIT);
            let deadline = std::time::Instant::now() + wait;
            loop {
                let row = match ctx.store.job(id) {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        return ToolOutcome::err(format!(
                            "no job {id} in this session: a job is named by the start a call \
                             answered with, and `check_bash` or `check_python` reads the ones \
                             their own shells started"
                        ));
                    }
                    Err(e) => return ToolOutcome::err(format!("could not read job {id}: {e}")),
                };
                // Another session's job is not this session's to read.
                if row.session != ctx.session {
                    return ToolOutcome::err(format!("no job {id} in this session"));
                }
                if row.status != "running" {
                    return ToolOutcome::ok(self.answer(ctx, &row));
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    let ran = crate::agent::spent(
                        u64::try_from((crate::store::now_millis() - row.started_at).max(0))
                            .unwrap_or(u64::MAX),
                    );
                    return ToolOutcome::ok(format!(
                        "job {id}{} is still running after {ran}. `wait_ms` stays in this call \
                         until it ends, and this session is told when it ends.",
                        named(&row)
                    ));
                }
                // Only a clock: a message or a cancel ends this from `waited`.
                tokio::time::sleep(remaining.min(WAIT_POLL)).await;
            }
        })
    }

    /// A job that ended does not stop: it has said all it will say.
    fn cut<'a>(
        &'a self,
        args: &'a Value,
        _ctx: &'a ToolContext,
        why: Cut,
        _started: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let id = args
                .get("job_id")
                .and_then(Value::as_u64)
                .map(|id| id.to_string())
                .unwrap_or_else(|| "?".to_string());
            ToolOutcome::err(format!(
                "check_job stopped waiting for job {id}: {}. The job was not stopped, and \
                 `check_job` reads what it answered.",
                why.because()
            ))
        })
    }
}

impl CheckJobTool {
    /// What one job says about itself, and whether that answer is still unread.
    fn answer(&self, ctx: &ToolContext, row: &crate::store::JobRow) -> String {
        let ran = row
            .finished_at
            .map(|done| {
                crate::agent::spent(
                    u64::try_from((done - row.started_at).max(0)).unwrap_or(u64::MAX),
                )
            })
            .unwrap_or_default();
        let what = format!("job {}{}", row.id, named(row));
        let said = match row.status.as_str() {
            "ok" => format!("{what} finished successfully after {ran}"),
            "error" => format!("{what} finished with an error after {ran}"),
            "lost" => format!(
                "{what} never reported: the server stopped before it finished, so nothing here \
                 can say what it printed. Run it again if it mattered."
            ),
            other => format!("{what} is {other}"),
        };
        if row.collected {
            return format!(
                "{said}. Its answer has already been collected: it is in the result of the \
                 `check_job` that read it."
            );
        }
        let Some(output) = row.output.as_deref() else {
            return format!("{said}, and it left nothing to read.");
        };
        if row.status == "lost" {
            return said;
        }
        let text = ctx.cap(
            ctx.machine,
            "check_job",
            None,
            output.to_string(),
            self.max_output_chars,
        );
        if let Err(e) = ctx.store.collect_job(row.id) {
            tracing::warn!(job = row.id, error = %e, "could not mark a job's answer collected");
        }
        format!("{said}. What it printed:\n\n{text}")
    }
}

/// How a job names itself in a sentence: its tool, and the call's own title.
fn named(row: &crate::store::JobRow) -> String {
    match &row.description {
        Some(description) if !description.trim().is_empty() => {
            format!(" (`{}`, \"{}\")", row.tool, description.trim())
        }
        _ => format!(" (`{}`)", row.tool),
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

impl Tool for WaitTool {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn default_timeout(&self) -> Option<Duration> {
        Some(self.timeout)
    }

    fn tunneled(&self) -> bool {
        false
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "timeout_ms": {"type": "integer"}
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
            let began = std::time::Instant::now();
            let deadline = began + wait;
            loop {
                // Read first: a cancel that came before this call is not a reason to wait.
                if ctx.stop.requested() {
                    return wait_cancelled(ctx, began.elapsed());
                }
                // The person's message ends this wait (see `cut`); rows are what
                // is left to watch.
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
                tokio::time::sleep(remaining.min(WAIT_POLL)).await;
                // The sleep is the whole poll: a `cancel` sets a flag, not this token.
                if ctx.stop.requested() {
                    return wait_cancelled(ctx, began.elapsed());
                }
            }
        })
    }

    /// The one tool whose whole point is the wait: being cut by the news is its answer.
    fn cut<'a>(
        &'a self,
        _args: &Value,
        _ctx: &'a ToolContext,
        why: Cut,
        _started: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            match why {
                Cut::Message => ToolOutcome::ok(
                    "a message arrived for this session while this call waited; it is \
                     delivered at the next step boundary",
                ),
                Cut::Cancelled => ToolOutcome::err("wait stopped: this turn was cancelled"),
            }
        })
    }
}

/// What a `wait` answers when a `cancel` ended it, and what it leaves behind.
///
/// # Invariant
///
/// A wait is not work that can be left undone: it started nothing, and the news it was
/// waiting for is still the server's to deliver. So the turn owes nothing afterwards --
/// `nothing_owed` says so, which is why the next start does not replay it -- and what
/// happened is recorded once, as a notice row the session reads whenever it next talks.
fn wait_cancelled(ctx: &ToolContext, waited: Duration) -> ToolOutcome {
    ctx.stop.nothing_owed();
    let message = format!(
        "This session was waiting for news when the server was asked to stop, {} into the \
         wait. It ended there, and it is not resumed: nothing was left undone, so the next \
         start owes nothing. What it was waiting for -- a message, a `subscribe` match, a \
         finished job -- is still the next start's to deliver.",
        crate::agent::spent(u64::try_from(waited.as_millis()).unwrap_or(u64::MAX))
    );
    if let Err(e) = ctx.store.add_pending_task(
        &ctx.session,
        "inject",
        &json!({
            "session": ctx.session,
            "message": message,
            "notice": "wait_cancelled",
        }),
    ) {
        tracing::warn!(session = %ctx.session, error = %e,
            "could not record a wait that a cancel ended");
    }
    ToolOutcome::ok(
        "the server is stopping: this wait ends here and is not resumed. Nothing you were \
         waiting for was stopped -- news that lands later is the next start's to deliver.",
    )
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

// --------------------------------- del_job ---------------------------------

/// SPEC's `del_job`: forget a job, and stop it if it is still running.
pub struct DelJobTool;

impl Tool for DelJobTool {
    fn name(&self) -> &'static str {
        "del_job"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {"type": "integer"}
            },
            "required": ["job_id"],
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(id) = args
                .get("job_id")
                .and_then(Value::as_u64)
                .and_then(|id| i64::try_from(id).ok())
            else {
                return ToolOutcome::err("del_job needs the `job_id` its start answered with");
            };
            let row = match ctx.store.job(id) {
                Ok(Some(row)) => row,
                Ok(None) => return ToolOutcome::err(format!("no job {id} in this session")),
                Err(e) => return ToolOutcome::err(format!("could not read job {id}: {e}")),
            };
            if row.session != ctx.session {
                return ToolOutcome::err(format!("no job {id} in this session"));
            }
            // Forgotten before it is stopped: a job that is cancelled writes its ending,
            // and an ending nobody asked for is not news.
            if let Err(e) = ctx.store.delete_job(id) {
                return ToolOutcome::err(format!("could not delete job {id}: {e}"));
            }
            let stopped = match row.pid {
                Some(pid) => {
                    // Armed: the job object ends the tree exactly. Unarmed: the walk.
                    let exact = crate::tools::detached::disarm_detached(pid as u32);
                    crate::tools::detached::kill_tree(pid as u32).await || exact
                }
                None => ctx.jobs.stop(id),
            };
            let what = named(&row);
            ToolOutcome::ok(if stopped {
                format!(
                    "deleted job {id}{what}, and stopped it: what it had printed is not kept, \
                     and nothing about it will be posted."
                )
            } else {
                format!("deleted job {id}{what}. It had already ended.")
            })
        })
    }
}

// -------------------------------- list_bash --------------------------------

pub struct ListBashTool;

impl Tool for ListBashTool {
    fn name(&self) -> &'static str {
        "list_bash"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name_regex": {"type": "string"}
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

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name_regex": {"type": "string"}
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
