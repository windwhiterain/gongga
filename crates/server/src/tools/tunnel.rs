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

/// Whether this call said `background`.
pub fn wants_background(args: &Value) -> bool {
    args.get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The word a tool the tunnel leaves alone may not say.
pub fn foreign(tool: &dyn Tool, args: &Value) -> Option<&'static str> {
    if tool.tunneled() {
        return None;
    }
    wants_background(args).then_some("background")
}

/// What carries a call that said `background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carried {
    /// The tool starts a process of its own and answers with what it started.
    Process,
    /// A worker process runs this call and leaves its answer in a file.
    Worker,
    /// Neither: it acts on what this server holds, so `background` is refused.
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

/// How a call that does not wait is recorded: one kind of job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The answer waits in the job's row until `check_job` reads it.
    Background,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        "background"
    }
}

/// Starts `name` as a worker process, and answers for it before it is done.
///
/// # Invariant
///
/// The request file is written and the pid recorded before the receipt leaves, so an
/// id the caller can name is one the next start can adopt: the worker outlives this
/// server, and so must everything needed to collect what it answered.
pub async fn spawn_worker(
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
        object.remove("background");
    }
    let id = match ctx.store.add_job(
        &ctx.session,
        &ctx.call_id,
        &name,
        description.as_deref(),
        Mode::Background.as_str(),
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
        explore: ctx.explore.is_some(),
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
    let mut sink = ctx.clone();
    // The turn that asked for this stopped waiting: its stream is not this job's.
    sink.events = None;
    let named = name.clone();
    tokio::spawn(async move {
        watch(sink, id, named, answer_path, pid, child, max).await;
    });
    ToolOutcome::ok(format!(
        "`{name}` started as job {id}: it runs in a process of its own, so it outlives \
         this server and is picked up again if the server restarts. This session is told \
         when it ends, and `check_job` with `job_id: {id}` collects its answer."
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
    // The leader ended, so what it left in the background is let go, not ended.
    crate::tools::detached::release_ended(pid);
    let ran = crate::agent::spent(
        u64::try_from((crate::store::now_millis() - started).max(0)).unwrap_or(u64::MAX),
    );
    let (ok, text) = match crate::worker::read(&answer_path) {
        Some(answer) => (answer.ok, answer.output),
        // What it printed is the answer when the answer file was never written.
        None => {
            let printed =
                std::fs::read_to_string(answer_path.with_extension("log")).unwrap_or_default();
            let why = format!(
                "the process ended without leaving an answer; what it printed is in {}",
                answer_path.with_extension("log").display()
            );
            if printed.trim().is_empty() {
                (false, why)
            } else {
                (false, format!("{printed}\n[{why}]"))
            }
        }
    };
    let capped = match crate::tools::cap_max_output(&ctx, &name, None, &ctx.call_id, &text, max) {
        Some(metadata) => format!("{metadata}\n(over {max} chars: it is all in that file)"),
        None => text,
    };
    report(&ctx, id, &name, &capped, ok, &ran).await;
}

/// Runs one program as a process of its own, and waits for it until `clock`.
///
/// # Invariant
///
/// The row exists from the start and is deleted only when the call answered: a call
/// still running when this server stops is a job the next start adopts, and one cut by
/// anything that is not a `cancel` takes its program with it. See
/// `docs/notes/server-tools-exec.md`.
pub async fn run_own_program(
    ctx: &ToolContext,
    name: &str,
    description: Option<&str>,
    max: usize,
    program: &std::path::Path,
    args: &[String],
    cwd: &std::path::Path,
    env: &[(String, String)],
    clock: std::time::Duration,
) -> Result<crate::tools::words::RunOutcome, ToolOutcome> {
    let log = ctx.output_dir.join(format!("call-{}.log", ctx.call_id));
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let own = match crate::tools::words::spawn_own(program, args, cwd, env, &log) {
        Ok(own) => own,
        Err(e) => return Err(ToolOutcome::err(e)),
    };
    let id = match ctx.store.add_job(
        &ctx.session,
        &ctx.call_id,
        name,
        description,
        Mode::Background.as_str(),
        crate::store::now_millis(),
    ) {
        Ok(id) => id,
        Err(e) => {
            let mut child = own.child;
            let _ = child.start_kill();
            return Err(ToolOutcome::err(format!(
                "`{name}` ran nothing: this call could not be recorded as a job ({e}), and a \
                 process nothing records is one whose answer nobody could collect."
            )));
        }
    };
    let mut running = Running {
        ctx,
        id,
        pid: own.pid,
        log: own.log,
        child: Some(own.child),
        done: false,
    };
    let status = {
        let child = running.child.as_mut().expect("just started");
        tokio::time::timeout(clock, child.wait()).await
    };
    match status {
        Ok(Ok(status)) => {
            running.done = true;
            let printed = std::fs::read_to_string(&running.log).unwrap_or_default();
            crate::tools::detached::release_ended(running.pid);
            let _ = ctx.store.delete_job(running.id);
            // In hand, so the log is a copy: only a call that becomes a job keeps it.
            let _ = std::fs::remove_file(&running.log);
            Ok(crate::tools::words::RunOutcome {
                stdout: printed,
                stderr: String::new(),
                code: status.code().unwrap_or(-1),
            })
        }
        Ok(Err(e)) => Err(ToolOutcome::err(format!(
            "waiting for {}: {e}",
            program.display()
        ))),
        Err(_) => {
            running.done = true;
            let child = running.child.take();
            Err(promote_child(
                ctx,
                name,
                max,
                running.id,
                running.pid,
                running.log.clone(),
                child,
                clock,
            ))
        }
    }
}

/// A call's own program, and what becomes of it when the call ends without it.
struct Running<'a> {
    ctx: &'a ToolContext,
    id: i64,
    pid: u32,
    log: std::path::PathBuf,
    child: Option<tokio::process::Child>,
    /// Set once the call has answered, or handed the work to the job table.
    done: bool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // A `cancel` lets it go; every other cut drops the work.
        if crate::commands::cancel_lets_go() {
            return;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        let _ = std::fs::remove_file(&self.log);
        let _ = self.ctx.store.delete_job(self.id);
    }
}

/// Hands a program that outlived its wait to the job table, and answers for it.
///
/// # Invariant
///
/// The row names the process and the file its answer will be read from before the
/// receipt leaves, so the next start can adopt a call this server never saw end.
fn promote_child(
    ctx: &ToolContext,
    name: &str,
    max: usize,
    id: i64,
    pid: u32,
    log: std::path::PathBuf,
    child: Option<tokio::process::Child>,
    clock: std::time::Duration,
) -> ToolOutcome {
    let answer_path = log.with_extension("answer.json");
    if let Err(e) = ctx.store.set_job_process(id, pid, &answer_path) {
        return ToolOutcome::err(format!(
            "`{name}` is still running, and the job it became could not be pointed at its \
             process ({e}),             so nothing would collect what it prints."
        ));
    }
    let mut sink = ctx.clone();
    // The call that asked for this stopped waiting: its stream is not this job's.
    sink.events = None;
    let named = name.to_string();
    tokio::spawn(async move {
        let code = match child {
            Some(mut child) => match child.wait().await {
                Ok(status) => status.code(),
                Err(_) => None,
            },
            None => None,
        };
        crate::tools::detached::release_ended(pid);
        let mut text = std::fs::read_to_string(&log).unwrap_or_default();
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "[exit status {}]",
            code.map_or_else(|| "unknown".to_string(), |c| c.to_string())
        ));
        let _ = crate::worker::write(
            &answer_path,
            &crate::worker::Answer {
                ok: code == Some(0),
                output: text,
            },
        );
        watch(sink, id, named, answer_path, pid, None, max).await;
    });
    ToolOutcome::ok(format!(
        "`{name}` was still running after {}, so this call became background job {id}: it runs \
         in a process of its own, so it outlives this server and is adopted on the next start. \
         `check_job` with `job_id: {id}` collects what it prints.",
        crate::agent::spent(u64::try_from(clock.as_millis()).unwrap_or(u64::MAX))
    ))
}

/// Writes how the job ended, and the notice that says so, in one act.
///
/// # Invariant
///
/// The row is written even when nobody is left to tell: what a session was owed must
/// outlive the session's silence, and a deleted session owes nothing. The answer
/// waits in the row until `check_job` reads it.
pub(crate) async fn report(
    ctx: &ToolContext,
    id: i64,
    name: &str,
    text: &str,
    ok: bool,
    spent: &str,
) {
    let status = if ok { "ok" } else { "error" };
    let what = match ok {
        true => "finished successfully",
        false => "finished with an error",
    };
    // A deleted session owes nothing, and a job the exit ended did not finish.
    let speaking = !matches!(ctx.store.get_session(&ctx.session), Ok(None))
        && !ctx.closing.load(std::sync::atomic::Ordering::SeqCst);
    let message = format!(
        "Job {id} on `{name}` has {what} after {spent}. `check_job` with \
         `job_id: {id}` collects what it printed."
    );
    let notice =
        speaking.then(|| json!({"session": ctx.session, "message": message, "notice": "job"}));
    // A job that ran asynchronously ended here, so its writes are seen too.
    crate::explore::after_job(ctx, name);
    let collected = false;
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
/// A `background` job is a process of its own, so the next start adopts it rather than
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
                            "adopted a background call still running");
                        // Armed on proof only: a job object kills what it holds.
                        match crate::tools::detached::names_process(pid as u32, row.started_at) {
                            Ok(true) => crate::tools::detached::arm_adopted(pid as u32),
                            Ok(false) => tracing::warn!(session = %row.session, job = row.id, pid,
                                "not arming an adopted job whose pid now names another process"),
                            Err(e) => tracing::warn!(session = %row.session, job = row.id, pid,
                                error = %e,
                                "could not verify an adopted job, so it is watched unarmed"),
                        }
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
                            report(&ctx, row.id, &row.tool, &answer.output, answer.ok, &spent)
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
        // This context reads an answer and writes a row; it is no tool's own call.
        tool: String::new(),
        channels: Arc::new(crate::tools::channel::Channels::without_words(
            cfg,
            store.clone(),
            cfg.home.join("channels"),
        )),
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
        // The row a stopped run left is an answer that exists: nothing to look at.
        explore: None,
    }
}
