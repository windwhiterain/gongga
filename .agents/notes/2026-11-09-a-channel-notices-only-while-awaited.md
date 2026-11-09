# A channel notices the session only while it is waited on

User's ruling, 2026-11-09: `new`/`check` set a **wait flag** on the channel; the channel
notices the session when it has the flag and the program finished or its output matched
the `until`; then the flag is cleared.

## What it fixed

Every `new`/`check` was followed one step later by "The channel … has finished
successfully", even when the call's own answer already carried `exit: 0`. Two writers
observe one end -- the call (`talk` -> `answer`) and the watcher (`Channels::watch`) --
and the guard meant to silence the second could not fire: `Store::finish_channel`
answered `Ok(())` whether or not `WHERE ended_at IS NULL` moved a row, so `is_err()` was
dead code and the notice was effectively unconditional.

## Shape

- `channels.wait_marker TEXT`: the marker the waiting call has to see, `""` for the end
alone, `NULL` for nobody waiting. In the row, so an offer to notice survives a restart.
- `talk` sets it when it starts waiting and clears it (`take_channel_wait`) when its own
answer delivered the event; a call that gives up (`wait` expired, `wait: 0`, `interrupt`)
leaves it set, so the answer still arrives once, after the call is gone.
- `watch` skips a channel with no flag, and one a call is inside (in-memory attach set),
then claims the flag: clearing it is the token that lets exactly one teller speak. It now
answers a match as well as an end (`ChannelEnd.matched`).
- `finish_channel` returns whether it moved the row, so the store stays the arbiter of
who records an end.

Consequence worth stating: a channel left running after a call returned on its `until` is
silent until something is waited on again. SPEC's "a channel finished in background will
also notice the caller" is now read as the flag, not as the absence of a call.
