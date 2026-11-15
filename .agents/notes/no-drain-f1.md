# F1: cancel "drain wait" is a documentation ghost

Branch: no-drain. User verdict: change the docs, and wait for nothing. The code
already never waits; this makes "no wait" a structural fact and the claims true.
Zero behavior change.

## Verified mechanism (HEAD 8eb7febc)

- Dispatch::active / ActiveGuard: write-only counter, no .load anywhere. Dead.
- Real stop: cancel sets shutting_down, interrupts running turns, records
  server_stop, spawns a task sleeping SHUTDOWN_NOTICE (250ms) then pulls shutdown.
  net::serve checks the token each DRAIN_TICK (100ms) and breaks. lib.rs then:
  stop_watching.cancel -> matcher.await -> detached_watch.await -> sessions.closing
  -> scripts.stop_all -> exit. Unfinished turns survive as durable pending rows.
- matcher.await waits for the Subscriptions::watch LOOP only; run_action tasks are
  detached spawns nothing awaits. A stop does NOT wait for subscription actions;
  their news survives via the pending row send_message writes.
- SHUTDOWN_NOTICE is the ACK window, DRAIN_TICK the token poll cadence. Both kept.

## Scope

Code: commands.rs, subscriptions.rs, lib.rs, net.rs:253 comment. Docs:
server-commands.md, server-net.md, server-subscriptions.md, server-signal.md.
server-main.md clean; server-lib.md drain is the real runtime-drop wait (leave);
server-dream.md:100 already honest (leave). client/protocol draining flag untouched.
