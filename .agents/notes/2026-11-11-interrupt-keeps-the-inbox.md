# interrupt keeps the inbox

2026-11-11. User verdict: **不允许删除inbox** — a stop must never delete a session's inbox.
Follow-up verdict: an interrupt must not be immediately re-woken by that inbox — save it
to context and stop the loop, do not start a new turn for it.

## The bug

`send_message` to a busy session writes the message only as a `pending_tasks` row of
kind `inject` (the inbox); it enters the transcript at the next step boundary
(`deliver_inbox`). `interrupt_session` fired the token (the turn breaks before that
boundary) and then `clear_pending_tasks_except(name, "turn")` deleted every non-`turn`
row — including the `inject` row. The message was never in the transcript and its only
durable copy was deleted, so it was lost. `cancel` shared the same clearing.

This contradicted SPEC (`interrupt_session`: "everything received must be saved to
context"; invariant "everything must store and send") and the repo's own documented
promise that a stranded inbox row is delivered by `nudge_inboxes`.

## The fix

`commands::interrupt` now clears only what the session no longer owes, never the inbox:

- `Delete`: clears everything (the whole session is going away).
- busy `Cancel`/`Interrupt`: clears nothing here — the turn's own row is `finish_turn`'s
  to keep (cancel) or drop (interrupt); the inbox stays.
- idle `Cancel`/`Interrupt`: clears the leftover `turn` row, keeps `inject`.

The inbox is then saved to context without owing a new turn: as the interrupted turn
ends, `turn_inner` delivers the inbox while the gate is still held (after the interrupted
answer's row), so `nudge_inboxes` finds nothing to wake. An idle interrupt folds the inbox
in from `interrupt()` itself (no turn is running to do it).

An earlier version left the inbox pending and let `nudge_inboxes` deliver it — but that
re-woke the session right after the user stopped it, contradicting "stop the loop".
Folding it in at interrupt is the correction: saved to context, no new turn. A `cancel`
owes the turn to the next start, so it leaves the inbox pending instead.
`resume_pending`'s dropping of dead work's `background`/`job` notices is a separate,
deliberate contract and is unchanged.

## Verification

L3 (real server + fake slow-stream LLM + sqlite): `.dev/inbox_context_selftest.sh` —
queue a message mid-request, interrupt, assert it is saved to the transcript, no
inject/turn row left pending, no second model request, session stays idle. 3 consecutive
PASS. Result: `.dev/verify/<sha>-interrupt-inbox-context.json`.
