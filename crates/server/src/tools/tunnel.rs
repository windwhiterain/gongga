//! The tunnel: the arguments every tool call shares, resolved once.
//!
//! See `docs/notes/server-tools-tunnel.md`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::store::Store;
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// Reads `timeout_ms` for every tool: how long this call holds the turn.
///
/// # Invariant
///
/// One reading of how long a call may wait, so no two tools disagree about the
/// ceiling, and a call that asks for more is held to it rather than refused.
/// `wait` is the one tool the tunnel leaves alone.
pub fn resolve(tool: &dyn Tool, args: &mut Value, cap: Duration) {
    if !tool.tunneled() {
        return;
    }
    let Some(object) = args.as_object_mut() else {
        // Arguments that are not an object are refused by the call's own schema.
        return;
    };
    let wait = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .or_else(|| tool.default_timeout())
        .unwrap_or(cap)
        // A tool's own default is a config value, so the ceiling binds it too.
        .min(cap);
    object.insert(
        "timeout_ms".to_string(),
        Value::from(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
    );
}

/// The wait this call may take, as the tunnel resolved it.
///
/// # Invariant
///
/// A tool reads its wait from here rather than trusting the duration it was built
/// with: the built-in one is the default `resolve` starts from, not a second answer
/// that could exceed the ceiling.
pub fn wait_of(args: &Value) -> Option<Duration> {
    args.get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
}

/// Whether this call said `detached`.
pub fn wants_detached(args: &Value) -> bool {
    args.get("detached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether this call said `background`.
pub fn wants_background(args: &Value) -> bool {
    args.get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether a call that does not wait asked for both, which is no way to run it.
///
/// # Invariant
///
/// One refusal for every tool: `background` keeps the answer for `check_job` and
/// `detached` hands it over, so a call that says both has asked for two answers.
pub fn both_ways(args: &Value) -> bool {
    wants_background(args) && wants_detached(args)
}

/// The word a tool the tunnel leaves alone may not say.
pub fn foreign(tool: &dyn Tool, args: &Value) -> Option<&'static str> {
    if tool.tunneled() {
        return None;
    }
    if wants_background(args) {
        Some("background")
    } else if wants_detached(args) {
        Some("detached")
    } else {
        None
    }
}

/// What carries a call that said `detached`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carried {
    /// The tool starts a process of its own and answers with what it started.
    Process,
    /// A worker process runs this call and leaves its answer in a file.
    Worker,
    /// Neither: it acts on what this server holds, so it cannot be detached.
    Here,
}

/// The jobs this server is running, so one of them can be stopped by name.
///
/// # Invariant
///
/// Only a job of this process is here: a job of a run that stopped has no token to
/// cancel, and `del_job` answers for it out of its row. A token leaves as soon as its
/// job ends, so the map holds what is running and nothing else.
#[derive(Clone, Default)]
pub struct JobRegistry {
    running: Arc<std::sync::Mutex<std::collections::HashMap<i64, CancellationToken>>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the id a job was given, under the token that ends it.
    pub fn add(&self, id: i64, cancel: CancellationToken) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, cancel);
    }

    /// Forgets a job that ended by itself.
    pub fn done(&self, id: i64) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    /// Stops one job here; false when it had already ended.
    pub fn stop(&self, id: i64) -> bool {
        let cancel = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        match cancel {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }
}

/// Which way a call that does not wait gives up its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The answer waits in the job's row until `check_job` reads it.
    Background,
    /// The answer is handed to the session as a notice when the job ends.
    Detached,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Detached => "detached",
        }
    }
}

/// Starts the call as a job of this server and answers for it before it is done.
///
/// # Invariant
///
/// The job outlives the call, not the server: it takes its own cancellation and its
/// own inbox, because the turn that asked for it ends long before it does, and it
/// writes no stream, because that stream belonged to the turn. What it keeps is the
/// session's shells, its interpreters and its store.
pub async fn spawn(
    tool: Arc<dyn Tool>,
    name: String,
    mut args: Value,
    ctx: &ToolContext,
    mode: Mode,
) -> ToolOutcome {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let max = args
        .get("max_output")
        .and_then(Value::as_u64)
        .map(|max| usize::try_from(max).unwrap_or(usize::MAX))
        .unwrap_or(crate::config::DEFAULT_MAX_OUTPUT_CHARS);
    // The tool is not told either word: what it runs is the work, not the waiting.
    if let Some(object) = args.as_object_mut() {
        object.remove("background");
        object.remove("detached");
    }
    // A job nothing records is one whose answer nobody could collect: it is not started.
    let id = match ctx.store.add_job(
        &ctx.session,
        &ctx.call_id,
        &name,
        description.as_deref(),
        mode.as_str(),
        crate::store::now_millis(),
    ) {
        Ok(id) => id,
        Err(e) => {
            return ToolOutcome::err(format!(
                "`{name}` ran nothing: this call could not be recorded as a job ({e}), and a \
                 job nothing records is one whose answer nobody could collect."
            ));
        }
    };
    let mut job = ctx.clone();
    job.cancel = CancellationToken::new();
    job.events = None;
    job.inbox = crate::session::InboxSignal::never();
    job.holds_the_turn = false;
    let started = std::time::Instant::now();
    let called = name.clone();
    let ended = job.cancel.clone();
    ctx.jobs.add(id, ended.clone());
    tokio::spawn(async move {
        let outcome = crate::tools::invoke(&called, tool, args, &job).await;
        job.jobs.done(id);
        let spent = crate::agent::spent(started.elapsed().as_millis() as u64);
        let text = match crate::tools::cap_max_output(
            &job,
            &called,
            description.as_deref(),
            &job.call_id,
            &outcome.output,
            max,
        ) {
            Some(metadata) => format!("{metadata}\n(over {max} chars: it is all in that file)"),
            None => outcome.output,
        };
        report(&job, id, &called, &text, outcome.ok, &spent, mode).await;
    });
    ToolOutcome::ok(match mode {
        Mode::Background => format!(
            "`{name}` started as job {id}: it runs on without this turn. `check_job` with \
             `job_id: {id}` collects what it answers, and this session is told when it ends."
        ),
        Mode::Detached => format!(
            "`{name}` started detached as job {id}: it runs on without this turn, and this \
             session is told when it ends. `wait` is how you stay in this turn until that \
             news lands."
        ),
    })
}

/// Starts `name` as a worker process, and answers for it before it is done.
///
/// # Invariant
///
/// The request file is written and the pid recorded before the receipt leaves, so an
/// id the caller can name is one the next start can adopt: the worker outlives this
/// server, and so must everything needed to collect what it answered.
pub async fn detach_to_worker(
    _tool: Arc<dyn Tool>,
    name: String,
    mut args: Value,
    ctx: &ToolContext,
) -> ToolOutcome {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let max = args
        .get("max_output")
        .and_then(Value::as_u64)
        .map(|max| usize::try_from(max).unwrap_or(usize::MAX))
        .unwrap_or(crate::config::DEFAULT_MAX_OUTPUT_CHARS);
    // The worker is told the work, not the waiting.
    if let Some(object) = args.as_object_mut() {
        object.remove("detached");
        object.remove("background");
    }
    let id = match ctx.store.add_job(
        &ctx.session,
        &ctx.call_id,
        &name,
        description.as_deref(),
        Mode::Detached.as_str(),
        crate::store::now_millis(),
    ) {
        Ok(id) => id,
        Err(e) => {
            return ToolOutcome::err(format!(
                "`{name}` ran nothing: this call could not be recorded as a job ({e})."
            ));
        }
    };
    let stem = ctx.output_dir.join(format!("job-{id}"));
    let request_path = stem.with_extension("request.json");
    let answer_path = stem.with_extension("answer.json");
    let log_path = stem.with_extension("log");
    let request = crate::worker::Request {
        home: ctx.home.clone(),
        session: ctx.session.clone(),
        call_id: ctx.call_id.clone(),
        tool: name.clone(),
        args,
        cwd: ctx.cwd.clone(),
        env: ctx.env.clone(),
        answer: answer_path.clone(),
    };
    let written = std::fs::create_dir_all(&ctx.output_dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            serde_json::to_vec(&request)
                .map_err(|e| e.to_string())
                .and_then(|bytes| std::fs::write(&request_path, bytes).map_err(|e| e.to_string()))
        });
    if let Err(e) = written {
        let _ = ctx.store.delete_job(id);
        return ToolOutcome::err(format!(
            "`{name}` ran nothing: its request could not be written ({e})."
        ));
    }
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => {
            let _ = ctx.store.delete_job(id);
            return ToolOutcome::err(format!(
                "`{name}` ran nothing: this binary's own path is unknown ({e})."
            ));
        }
    };
    let words = vec!["--run-tool".to_string(), request_path.display().to_string()];
    let mut launched = match crate::tools::detached::DetachedJob::launch_program(
        &program, &words, &ctx.cwd, &log_path, &ctx.env,
    ) {
        Ok(launched) => launched,
        Err(e) => {
            let _ = ctx.store.delete_job(id);
            return ToolOutcome::err(format!("`{name}` ran nothing: {e}."));
        }
    };
    let pid = launched.pid;
    if let Err(e) = ctx.store.set_job_process(id, pid, &answer_path) {
        let _ = ctx.store.delete_job(id);
        return ToolOutcome::err(format!(
            "`{name}` ran nothing: the process it started could not be recorded ({e})."
        ));
    }
    let child = launched.take_child();
    let sink = ctx.clone();
    let named = name.clone();
    tokio::spawn(async move {
        watch(sink, id, named, answer_path, pid, child, max).await;
    });
    ToolOutcome::ok(format!(
        "`{name}` started detached as job {id}: it runs in a process of its own, so it \
         outlives this server and is picked up again if the server restarts. This session \
         is told when it ends, and `check_job` with `job_id: {id}` collects its answer."
    ))
}

/// Waits for one worker to end, then writes what it answered where `check_job` reads it.
///
/// # Invariant
///
/// The answer is a file, so a worker that ends while this server is down still answers:
/// the waiter that adopts such a row reads the same file. Nothing here is invented from
/// what the process left on its log.
pub(crate) async fn watch(
    ctx: ToolContext,
    id: i64,
    name: String,
    answer_path: std::path::PathBuf,
    pid: u32,
    child: Option<tokio::process::Child>,
    max: usize,
) {
    let started = ctx
        .store
        .job(id)
        .ok()
        .flatten()
        .map(|row| row.started_at)
        .unwrap_or_else(crate::store::now_millis);
    match child {
        // Started here: its own handle is the process, so waiting on it is exact.
        Some(mut child) => {
            let _ = child.wait().await;
        }
        // Adopted: nothing can be waited on, so the process is asked.
        None => loop {
            match crate::tools::detached::alive_pids(&[pid]) {
                Ok(gone) if gone.is_empty() => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(job = id, pid, error = %e, "cannot ask about an adopted job");
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        },
    }
    let ran = crate::agent::spent(
        u64::try_from((crate::store::now_millis() - started).max(0)).unwrap_or(u64::MAX),
    );
    let (ok, text) = match crate::worker::read(&answer_path) {
        Some(answer) => (answer.ok, answer.output),
        None => (
            false,
            format!(
                "the process ended without leaving an answer; what it printed is in \
                 {}",
                answer_path.with_extension("log").display()
            ),
        ),
    };
    let capped = match crate::tools::cap_max_output(&ctx, &name, None, &ctx.call_id, &text, max) {
        Some(metadata) => format!("{metadata}\n(over {max} chars: it is all in that file)"),
        None => text,
    };
    report(&ctx, id, &name, &capped, ok, &ran, Mode::Detached).await;
}

/// Writes how the job ended, and the notice that says so, in one act.
///
/// # Invariant
///
/// The row is written even when nobody is left to tell: what a session was owed must
/// outlive the session's silence, and a deleted session owes nothing. A `detached`
/// answer is the notice itself, so it counts as collected the moment that notice is
/// queued; a `background` answer waits for `check_job`.
pub(crate) async fn report(
    ctx: &ToolContext,
    id: i64,
    name: &str,
    text: &str,
    ok: bool,
    spent: &str,
    mode: Mode,
) {
    let status = if ok { "ok" } else { "error" };
    let what = match ok {
        true => "finished successfully",
        false => "finished with an error",
    };
    // A deleted session owes nothing, and a job the exit ended did not finish.
    let speaking = !matches!(ctx.store.get_session(&ctx.session), Ok(None))
        && !ctx.closing.load(std::sync::atomic::Ordering::SeqCst);
    let (message, kind) = match mode {
        // What the job is, not what the caller said: a call that outlived its clock
        // is as much this job as one that asked to be.
        Mode::Background => (
            format!(
                "Job {id} on `{name}` has {what} after {spent}. `check_job` with \
                 `job_id: {id}` collects what it printed."
            ),
            "job",
        ),
        Mode::Detached => (
            format!(
                "The `{name}` call you started `detached` has finished after {spent}. This is \
                 its answer, arriving after the call that asked for it:\n\n{text}"
            ),
            "detached",
        ),
    };
    let notice =
        speaking.then(|| json!({"session": ctx.session, "message": message, "notice": kind}));
    let collected = mode == Mode::Detached && notice.is_some();
    if let Err(e) = ctx
        .store
        .finish_job(id, status, text, collected, notice.as_ref())
    {
        tracing::warn!(session = %ctx.session, job = id, tool = %name, error = %e,
            "could not record how a job ended");
    }
}

/// Tells every session what a stopped run left owed: a job that never answered, and
/// an answer nobody read.
///
/// # Invariant
///
/// A `detached` job is a process of its own, so the next start adopts it rather than
/// mourning it, and only a job with nothing left to adopt is reported lost. The row is
/// the truth and the notice only points at it. See `docs/notes/server-tools-worker.md`.
pub async fn announce_owed(
    store: &Store,
    cfg: &crate::config::Config,
    output_dir: &std::path::Path,
) -> usize {
    let Ok(rows) = store.owed_jobs() else {
        tracing::warn!("could not read the jobs a previous run left");
        return 0;
    };
    let mut told = 0;
    for row in rows {
        if matches!(store.get_session(&row.session), Ok(None)) {
            if let Err(e) = store.delete_job(row.id) {
                tracing::warn!(session = %row.session, job = row.id, error = %e,
                    "could not delete a job whose session is gone");
            }
            continue;
        }
        let named = match &row.description {
            Some(description) if !description.trim().is_empty() => {
                format!(" (`{}`)", description.trim())
            }
            _ => String::new(),
        };
        if row.status == "running" {
            let ctx = adopted(store, cfg, output_dir, &row.session);
            if let (Some(pid), Some(answer)) = (row.pid, row.answer.as_deref()) {
                match crate::tools::detached::alive_pids(&[pid as u32]) {
                    // Still out there: it outlived us, so it is watched, not mourned.
                    Ok(alive) if !alive.is_empty() => {
                        tracing::info!(session = %row.session, job = row.id, pid,
                            "adopted a detached call still running");
                        let sink = ctx.clone();
                        let name = row.tool.clone();
                        let path = std::path::PathBuf::from(answer);
                        tokio::spawn(async move {
                            watch(sink, row.id, name, path, pid as u32, None, usize::MAX).await;
                        });
                        continue;
                    }
                    // Ended while no server was up: its answer is still the file it wrote.
                    _ => {
                        if let Some(answer) = crate::worker::read(std::path::Path::new(answer)) {
                            let spent = crate::agent::spent(
                                u64::try_from((crate::store::now_millis() - row.started_at).max(0))
                                    .unwrap_or(u64::MAX),
                            );
                            report(
                                &ctx,
                                row.id,
                                &row.tool,
                                &answer.output,
                                answer.ok,
                                &spent,
                                Mode::Detached,
                            )
                            .await;
                            told += 1;
                            continue;
                        }
                    }
                }
            }
            if let Err(e) = store.lose_job(row.id) {
                tracing::warn!(session = %row.session, job = row.id, error = %e,
                    "could not record a job the last run lost");
            }
            let waited = crate::agent::spent(
                u64::try_from((crate::store::now_millis() - row.started_at).max(0))
                    .unwrap_or(u64::MAX),
            );
            let message = format!(
                "Job {} (`{}`{named}) never reported: the server stopped before it finished, \
                 and nothing that outlived it is left to ask. It was started {waited} before \
                 this server was. Run it again if it mattered.",
                row.id, row.tool
            );
            if let Err(e) = store.add_pending_task(
                &row.session,
                "inject",
                &json!({"session": row.session, "message": message, "notice": "lost"}),
            ) {
                tracing::warn!(session = %row.session, job = row.id, error = %e,
                    "could not report a lost job");
                continue;
            }
            told += 1;
            continue;
        }
        let message = format!(
            "Job {} (`{}`{named}) ended before the server stopped and its answer was never \
             collected. `check_job` with `job_id: {}` still reads it.",
            row.id, row.tool, row.id
        );
        if let Err(e) = store.add_pending_task(
            &row.session,
            "inject",
            &json!({"session": row.session, "message": message, "notice": "job"}),
        ) {
            tracing::warn!(session = %row.session, job = row.id, error = %e,
                "could not report a job the last run left owed");
            continue;
        }
        told += 1;
    }
    told
}

/// What a job of a stopped run is written from: no sh

/// What a job of a stopped run is written from: no session of this process is behind it.
///
/// # Invariant
///
/// The context holds no shells and no interpreters, because there are none to hold: what
/// it is used for is reading one answer and writing one row.
fn adopted(
    store: &Store,
    cfg: &crate::config::Config,
    output_dir: &std::path::Path,
    session: &str,
) -> ToolContext {
    ToolContext {
        session: session.to_string(),
        call_id: String::new(),
        home: cfg.home.clone(),
        output_dir: output_dir.to_path_buf(),
        shells: Arc::new(std::sync::Mutex::new(crate::session::Shells::empty(
            cfg.bash.program.clone().unwrap_or_default(),
        ))),
        pythons: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        reader: None,
        machine: crate::session::Machine::Local,
        remote: None,
        detached: crate::tools::detached::DetachedRegistry::new(
            store.clone(),
            cfg.bash.program.clone(),
            cfg.home.join("detached"),
            Vec::new(),
        ),
        jobs: JobRegistry::new(),
        events: None,
        cancel: CancellationToken::new(),
        store: store.clone(),
        cwd: cfg.home.clone(),
        env: Vec::new(),
        vision: crate::llm::Vision {
            accepts_image: false,
            refusal: String::new(),
        },
        inbox: crate::session::InboxSignal::never(),
        holds_the_turn: false,
        stop: crate::session::StopSignal::idle(),
        closing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}
