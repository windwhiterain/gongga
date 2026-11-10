# A stop that ended the channel a session was waiting on left it asleep

`cancel` on a session inside `wait`, then a restart, and the session did nothing: no turn,
no notice read. The wait had ~120 s left on its clock and the server treated that clock as
a schedule.

## What the DB said after `cancel`

```
sessions: ('s', idle, canceled=0, waiting_until=now+120000)
pending:  turn
pending:  inject  notice=channel  {"id":"C1","exit":null,"output":"...the server stopped this channel..."}
```

So both halves of SPEC's cancel had happened: the wait's flag was written, and every
channel was ended with its notice queued (`stop_every` -> the `Notifier` in `lib.rs`, an
`inject` row).

## Why it did not wake

Both recovery passes skipped a session whose clock had not run out:

```rust
if waiting.get(&name).is_some_and(|until| *until > now) {
    continue;
}
```

`resume_pending` used it to defer the session to the clock loop (sleep the remainder, then
spawn), and `nudge_inboxes` used it so the 2 s ticker would not start a waiting session
early. Neither asked the one question that matters: **is the thing it was waiting for
already in the inbox?**

A `wait` ends on what arrives. The channel's end notice is exactly that, so a session left
waiting with rows queued is not waiting any more -- its clock was only the ceiling.

## The fix

Both guards now read the rows first: a waiting session with work to read has its flag
cleared and its turn started at once; only a session with nothing to read sits out the
time that was left. The clock loop also re-checks that its row is still there before
starting anything, so a wait somebody else answered is nobody else's to start.

## Measured: one home, two server starts

```
before restart : waiting_until = +120000 ms, pending = [turn, inject/channel]
restart        : "replaying work left pending session=s"  0.5s after start
after          : waiting_until = NULL, pending = [], the wait's result "0.674s" written
                 by the cancel itself, the notice in the transcript as an `inject` row
```

0.5 s instead of the 120 s that was left.
