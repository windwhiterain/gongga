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
         Start it `detached` and let the report come to you instead: `{name}` with \
         `detached: true` returns at once, keeps running, and this session is told when it \
         ends -- then `wait` is how you stay in this turn until that news arrives. A wait \
         that is already running is not lengthened by asking for more."
    )
}

/// Whether this call said `detached`.
pub fn wants_detached(args: &Value) -> bool {
    args.get("detached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// What carries a call that said `detached`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carried {
    /// The tool starts a process of its own and answers with what it started.
    Process,
    /// The call runs as a job of this server and reports when it is done.
    Call,
}

/// Starts the call as a job of this server and answers for it before it is done.
///
/// # Invariant
///
/// The job outlives the call, not the server: it takes its own cancellation, because
/// the turn that asked for it ends long before it does, and it writes no stream, because
/// the stream it would have written to belonged to that turn. The row it leaves is what
/// lets the next start say what a process that stopped lost. See
/// `docs/notes/server-tools-detached.md`.
pub async fn detach(
    tool: Arc<dyn Tool>,
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
    // The tool is not told the word: what it runs is the work, not the detachment.
    if let Some(object) = args.as_object_mut() {
        object.remove("detached");
    }
    let recorded = ctx
        .store
        .add_detached_call(&ctx.session, &ctx.call_id, &name, description.as_deref())
        .unwrap_or_else(|e| {
            tracing::warn!(
                session = %ctx.session, tool = %name, error = %e,
                "could not record a detached call; the next start cannot report it lost"
            );
            -1
        });
    let mut job = ctx.clone();
    job.cancel = CancellationToken::new();
    job.events = None;
    let call_id = ctx.call_id.clone();
    let started = std::time::Instant::now();
    let where_it_is = name.clone();
    tokio::spawn(async move {
        let outcome = crate::tools::invoke(&name, tool, args, &job).await;
        let spent = crate::agent::spent(started.elapsed().as_millis() as u64);
        let text = match crate::tools::cap_max_output(
            &job,
            &name,
            description.as_deref(),
            &call_id,
            &outcome.output,
            max,
        ) {
            Some(metadata) => format!("{metadata}\n(over {max} chars: it is all in that file)"),
            None => outcome.output,
        };
        report(&job, recorded, &name, &text, spent).await;
    });
    ToolOutcome::ok(format!(
        "`{where_it_is}` started detached: it runs on without this turn, and this session \
         is told when it ends. `wait` is how you stay in this turn until that news lands."
    ))
}

/// Deletes the call's row and posts what it answered, in that order.
///
/// # Invariant
///
/// One row in, one row out: the call's own trace is deleted in the same act that posts
/// its report, so a row left behind always means a report nobody could post. A session
/// that is gone owes nothing, and a server that is closing did not see it end.
async fn report(ctx: &ToolContext, recorded: i64, name: &str, text: &str, spent: String) {
    if recorded >= 0 {
        if let Err(e) = ctx.store.delete_detached_call(recorded) {
            tracing::warn!(session = %ctx.session, tool = %name, error = %e,
                "could not delete a finished detached call's row");
        }
    }
    if matches!(ctx.store.get_session(&ctx.session), Ok(None)) {
        return;
    }
    if ctx.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let message = format!(
        "The `{name}` call you started `detached` has finished after {spent}. This is its \
         answer, arriving after the call that asked for it:\n\n{text}"
    );
    if let Err(e) = ctx.store.add_pending_task(
        &ctx.session,
        "inject",
        &json!({"session": ctx.session, "message": message, "notice": "detached"}),
    ) {
        tracing::warn!(session = %ctx.session, tool = %name, error = %e,
            "could not post a detached call's answer");
    }
}

/// Tells every session what a previous run left detached and never answered.
///
/// # Invariant
///
/// A row here has no pid to adopt: the job was this server's own task, so a row that
/// survives a start is a call whose answer died with the process. Saying so is the only
/// thing that can be said about it, and saying it is required -- a session waiting on
/// that answer would otherwise wait for ever.
pub fn announce_lost(store: &Store) -> usize {
    let Ok(rows) = store.detached_calls() else {
        tracing::warn!("could not read the detached calls a previous run left");
        return 0;
    };
    let mut told = 0;
    for row in rows {
        if matches!(store.get_session(&row.session), Ok(Some(_))) {
            let named = match &row.description {
                Some(description) if !description.trim().is_empty() => {
                    format!(" (`{}`)", description.trim())
                }
                _ => String::new(),
            };
            let waiting_since = crate::agent::spent(
                u64::try_from((crate::store::now_millis() - row.started_at).max(0))
                    .unwrap_or(u64::MAX),
            );
            tracing::info!(
                session = %row.session, tool = %row.tool, call_id = %row.call_id,
                "reporting a detached call a stopped server never answered"
            );
            let message = format!(
                "The `{}` call{named} you started `detached` never reported: the server \
                 stopped before it finished, so nothing here can say what it printed. It \
                 was a job of that server, not a process of its own, so it did not outlive \
                 it -- and it was started {waiting_since} before this server was. Run it \
                 again if it mattered.",
                row.tool
            );
            if let Err(e) = store.add_pending_task(
                &row.session,
                "inject",
                &json!({"session": row.session, "message": message, "notice": "detached"}),
            ) {
                tracing::warn!(session = %row.session, error = %e,
                    "could not report a detached call that a stopped server lost");
                continue;
            }
            told += 1;
        }
        if let Err(e) = store.delete_detached_call(row.id) {
            tracing::warn!(session = %row.session, error = %e,
                "could not delete a dead detached call's row");
        }
    }
    told
}
