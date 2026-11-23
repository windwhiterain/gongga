//! The checkpoint commands: `live_compact`, `memorize`, `recall`.
//! See `SPEC.md` "Live Compact" and "Memory Mask".
//!
//! # Invariant
//!
//! A tool call is one invocation surface; the agent loop names no tool. The
//! command commits its checkpoint, stores and announces its result row, and
//! states the generic end-turn / stored-result signals the loop honors.

use protocol::Args;
use serde_json::json;

use crate::commands::{Dispatch, Origin, Outcome};
use crate::live_compact::CompactMode;
use crate::tools::live_compact::LIVE_COMPACT_TOOL;
use crate::tools::memory::{MEMORIZE_TOOL, RECALL_TOOL};

/// The session and model call a checkpoint anchors on.
///
/// # Invariant
///
/// A checkpoint folds the transcript at a tool call, so it needs one: an
/// operator line with no calling row is refused, not given an invented anchor.
fn anchor(
    d: &Dispatch,
    origin: &Origin,
) -> Result<(crate::session::SessionHandle, String), Outcome> {
    let Some(name) = origin.session() else {
        return Err(Outcome::err(
            "a checkpoint runs from a session, and none names this call",
        ));
    };
    let Some(call_id) = origin.call_id() else {
        return Err(Outcome::err(
            "a checkpoint folds the transcript at a model tool call: invoke it through \
             gongga_cli from a turn, not as a bare operator line",
        ));
    };
    let handle = d
        .sessions
        .get(name)
        .map_err(|e| Outcome::err(e.to_string()))?;
    Ok((handle, call_id.to_string()))
}

/// The result row a checkpoint stores, carrying its calling row's turn and step.
fn content(
    d: &Dispatch,
    id: i64,
    call_id: &str,
    expected: &str,
    output: &str,
    elapsed_ms: u64,
) -> Result<serde_json::Value, Outcome> {
    let conn = d.store.picture_conn();
    let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
    let call = crate::memory::stored_call(&conn, id, call_id, expected)
        .map_err(|e| Outcome::err(format!("{e:#}")))?;
    Ok(json!({
        "call_id": call_id,
        "output": output,
        "images": [],
        "turn": call.turn,
        "step": call.step,
        "ok": true,
        "elapsed_ms": elapsed_ms,
    }))
}

/// Announces a row the command stored, as the loop's own `announce_row` does.
fn announce(d: &Dispatch, name: &str, seq: i64) {
    d.bus.publish(protocol::Event::Transcript {
        session: name.to_string(),
        seq,
    });
}

/// `live_compact`: fold the transcript in cut mode; the commit reads
/// `next`/`continue` from the calling row.
pub async fn live_compact(d: &Dispatch, args: &Args, origin: &Origin) -> Outcome {
    let began = std::time::Instant::now();
    let (handle, call_id) = match anchor(d, origin) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let id = handle.id();
    let name = handle.name().to_string();
    let tuning = handle.tuning();
    let compact_mode = tuning
        .live_compact_mode
        .unwrap_or(d.agent.config.now().agent.live_compact_mode);
    if compact_mode != CompactMode::Cut {
        return Outcome::err("this session uses mask checkpoints; call memorize instead");
    }
    let settled = args.word("settled").unwrap_or("").trim();
    if settled.is_empty() {
        return Outcome::err("settled must not be empty");
    }
    let space = handle.space();
    let machines = crate::meta::Machines::of(&space);
    let embedded = crate::meta::embed(&machines, &handle.cwd(), settled).await;
    let content = match content(
        d,
        id,
        &call_id,
        LIVE_COMPACT_TOOL,
        &embedded,
        began.elapsed().as_millis() as u64,
    ) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match d.store.append_live_compact_result(id, &call_id, &content) {
        Ok(crate::live_compact::Commit::Checkpointed(committed)) => {
            announce(d, &name, committed.result_seq);
            d.agent.log_event(
                Some(id),
                "live_compact",
                &json!({
                    "checkpoint": committed.checkpoint_id,
                    "result_seq": committed.result_seq,
                    "generation": committed.generation,
                    "continue": committed.continue_turn,
                }),
            );
            Outcome::ok_with(
                embedded,
                json!({
                    "result_seq": committed.result_seq,
                    "end_turn": !committed.continue_turn,
                    "checkpoint": committed.checkpoint_id,
                    "generation": committed.generation,
                    "continue": committed.continue_turn,
                }),
            )
        }
        Ok(crate::live_compact::Commit::Nothing(reason)) => Outcome::ok(reason),
        Err(e) => Outcome::err(format!("live_compact was not committed: {e:#}")),
    }
}

/// The mask-mode guard shared by `memorize` and `recall`.
///
/// # Invariant
///
/// The session must be in mask mode on an endpoint declaring the memory-mask
/// api; returns the client and the visible-token ceiling for displacement.
fn mask_guard(
    d: &Dispatch,
    handle: &crate::session::SessionHandle,
) -> Result<(std::sync::Arc<crate::llm::LlmClient>, Option<u64>), Outcome> {
    let tuning = handle.tuning();
    let compact_mode = tuning
        .live_compact_mode
        .unwrap_or(d.agent.config.now().agent.live_compact_mode);
    if compact_mode != CompactMode::Mask {
        return Err(Outcome::err("requires live_compact.mode = mask"));
    }
    let (llm, _budget, _context) = d
        .agent
        .route(handle.name(), &tuning)
        .map_err(|e| Outcome::err(format!("{e:#}")))?;
    if !llm.supports_memory_mask() {
        return Err(Outcome::err(format!(
            "requires endpoint api {:?}",
            crate::llm::MEMORY_MASK_API
        )));
    }
    let token_limit = d.agent.memory_token_limit(&llm, &tuning);
    Ok((llm, token_limit))
}

/// `memorize`: commit a mask checkpoint without cutting the canonical tape.
pub async fn memorize(d: &Dispatch, args: &Args, origin: &Origin) -> Outcome {
    let began = std::time::Instant::now();
    let (handle, call_id) = match anchor(d, origin) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let id = handle.id();
    let name = handle.name().to_string();
    if let Err(e) = mask_guard(d, &handle) {
        return e;
    }
    let summary = args.word("summary").unwrap_or("").trim();
    if summary.is_empty() {
        return Outcome::err("summary must not be empty");
    }
    let space = handle.space();
    let machines = crate::meta::Machines::of(&space);
    let embedded = crate::meta::embed(&machines, &handle.cwd(), summary).await;
    let content = match content(
        d,
        id,
        &call_id,
        MEMORIZE_TOOL,
        &embedded,
        began.elapsed().as_millis() as u64,
    ) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match d.store.memorize(id, &call_id, &content) {
        Ok(crate::memory::MemorizeCommit::Checkpointed(committed)) => {
            announce(d, &name, committed.result_seq);
            d.agent.log_event(
                Some(id),
                "memorize",
                &json!({
                    "memory": committed.id,
                    "checkpoint": committed.checkpoint_id,
                    "result_seq": committed.result_seq,
                    "source_after_seq": committed.source_after_seq,
                    "cutoff_seq": committed.cutoff_seq,
                    "view_generation": committed.view_generation,
                }),
            );
            Outcome::ok_with(
                committed.output.clone(),
                json!({
                    "result_seq": committed.result_seq,
                    "memory": committed.id,
                    "checkpoint": committed.checkpoint_id,
                    "view_generation": committed.view_generation,
                }),
            )
        }
        Ok(crate::memory::MemorizeCommit::Nothing(reason)) => Outcome::ok(reason),
        Err(e) => Outcome::err(format!("memorize was not committed: {e:#}")),
    }
}

/// `recall`: unmask original spans, with FIFO displacement bounded by the window.
pub async fn recall(d: &Dispatch, _args: &Args, origin: &Origin) -> Outcome {
    let began = std::time::Instant::now();
    let (handle, call_id) = match anchor(d, origin) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let id = handle.id();
    let name = handle.name().to_string();
    let (_llm, token_limit) = match mask_guard(d, &handle) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let content = match content(
        d,
        id,
        &call_id,
        RECALL_TOOL,
        "",
        began.elapsed().as_millis() as u64,
    ) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match d.store.recall(id, &call_id, &content, token_limit) {
        Ok(committed) => {
            announce(d, &name, committed.result_seq);
            d.agent.log_event(
                Some(id),
                "recall",
                &json!({
                    "result_seq": committed.result_seq,
                    "recalled": committed.recalled,
                    "auto_masked": committed.auto_masked,
                    "view_generation": committed.view_generation,
                }),
            );
            Outcome::ok_with(
                committed.output.clone(),
                json!({
                    "result_seq": committed.result_seq,
                    "recalled": committed.recalled,
                    "auto_masked": committed.auto_masked,
                    "view_generation": committed.view_generation,
                }),
            )
        }
        Err(e) => Outcome::err(format!("recall was not committed: {e:#}")),
    }
}
