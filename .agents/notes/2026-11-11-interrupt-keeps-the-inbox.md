# interrupt keeps the inbox

2026-11-11. User verdict: **不允许删除inbox** — a stop must never delete a session's inbox.

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

A queued operator message therefore survives the stop, and `nudge_inboxes` (2s tick)
starts a turn that delivers it to the transcript — the same path a turn that ends first
takes. `resume_pending`'s dropping of dead work's `background`/`job` notices is a
separate, deliberate contract and is unchanged.

## Verification

L3 (real server + fake slow-stream LLM + sqlite): `.dev/inbox_selftest.sh` — queue a
message mid-request, interrupt, assert the inject row survives, assert it is delivered
to the transcript. PASS. Result: `.dev/verify/<sha>-interrupt-inbox.json`.
