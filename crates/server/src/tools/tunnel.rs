//! The tunnel: the arguments every tool call shares, resolved once.
//!
//! See `docs/notes/server-tools-tunnel.md`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::store::Store;
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// Reads `timeout_ms` for every tool, and refuses a wait this tunnel will not hold.
///
/// # Invariant
///
/// One reading of how long a call may wait: no tool decides that for itself, so no
/// two tools can disagree about the ceiling, and a call that names a longer wait is
/// told what to do instead of being silently shortened. `wait` is the one tool the
/// tunnel leaves alone -- waiting is its whole work, and a ceiling on it would keep
/// it from waiting for what it was asked to wait for.
pub fn resolve(tool: &dyn Tool, args: &mut Value, cap: Duration) -> Result<(), String> {
    if !tool.tunneled() {
        return Ok(());
    }
    let Some(object) = args.as_object_mut() else {
        // Arguments that are not an object are refused by the call's own schema.
        return Ok(());
    };
    let stated = object.get("timeout_ms").and_then(Value::as_u64);
    if let Some(asked) = stated
        && Duration::from_millis(asked) > cap
    {
        return Err(too_long(tool.name(), Duration::from_millis(asked), cap));
    }
    let wait = stated
        .map(Duration::from_millis)
        .or_else(|| tool.default_timeout())
        .unwrap_or(cap)
        // A tool's own default is a config value, so the ceiling binds it too.
        .min(cap);
    object.insert(
        "timeout_ms".to_string(),
        Value::from(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
    );
    Ok(())
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

/// Why one call does not get the wait it asked for, and what to do instead.
///
/// # Invariant
///
/// The ceiling is not a failure of the call's work: it is a refusal to hold the turn
/// for it, so the sentence always names the way to run the same work without waiting.
fn too_long(name: &str, asked: Duration, cap: Duration) -> String {
    let asked = crate::agent::spent(u64::try_from(asked.as_millis()).unwrap_or(u64::MAX));
    let cap = crate::agent::spent(u64::try_from(cap.as_millis()).unwrap_or(u64::MAX));
    format!(
        "`{name}` did not run: it asked to wait {asked} and no call waits longer than {cap}. \
         Start the work so that it reports to you instead -- `background` answers at once \
         with a `job_id` that `check_job` collects, and `detached` hands the answer over \
         when it lands -- or ask again later. A wait that is already running is not \
         lengthened by asking for more."
    )
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
    /// The call runs as a job of this server and reports when it is done.
    Call,
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
    let started = std::time::Instant::now();
    let called = name.clone();
    tokio::spawn(async move {
        let outcome = crate::tools::invoke(&called, tool, args, &job).await;
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

/// Writes how the job ended, and the notice that says so, in one act.
///
/// # Invariant
///
/// The row is written even when nobody is left to tell: what a session was owed must
/// outlive the session's silence, and a deleted session owes nothing. A `detached`
/// answer is the notice itself, so it counts as collected the moment that notice is
/// queued; a `background` answer waits for `check_job`.
async fn report(
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
        Mode::Background => (
            format!(
                "Job {id} you started with `background` on `{name}` has {what} after {spent}. \
                 `check_job` with `job_id: {id}` collects what it printed."
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
/// The row is the truth and the notice only points at it, so a session told about a
/// job finds the same verdict when it asks `check_job`. A job whose session is gone is
/// forgotten rather than announced.
pub fn announce_owed(store: &Store) -> usize {
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
        let (message, kind) = match row.status.as_str() {
            "running" => {
                // Nothing can adopt it: a job of a process died with the process.
                if let Err(e) = store.lose_job(row.id) {
                    tracing::warn!(session = %row.session, job = row.id, error = %e,
                        "could not record a job the last run lost");
                }
                let waited = crate::agent::spent(
                    u64::try_from((crate::store::now_millis() - row.started_at).max(0))
                        .unwrap_or(u64::MAX),
                );
                (
                    format!(
                        "Job {} (`{}`{named}) never reported: the server stopped before it \
                         finished, so nothing here can say what it printed. It was a job of that \
                         server, not a process of its own, so it did not outlive it -- and it was \
                         started {waited} before this server was. Run it again if it mattered.",
                        row.id, row.tool
                    ),
                    "lost",
                )
            }
            _ => (
                format!(
                    "Job {} (`{}`{named}) you started `{}` ended before the server stopped and \
                     its answer was never collected. `check_job` with `job_id: {}` still reads it.",
                    row.id, row.tool, row.mode, row.id
                ),
                "job",
            ),
        };
        if let Err(e) = store.add_pending_task(
            &row.session,
            "inject",
            &json!({"session": row.session, "message": message, "notice": kind}),
        ) {
            tracing::warn!(session = %row.session, job = row.id, error = %e,
                "could not report a job the last run left owed");
            continue;
        }
        told += 1;
    }
    told
}
