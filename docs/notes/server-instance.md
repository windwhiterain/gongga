# server/instance.rs

## instance

One server per home directory.

Two servers sharing one home is a state nothing here has an answer for: they
share the SQLite file, so each would resume the same pending turn, compress the
same transcript, and drive the same session at the same time. Binding the UDP
port already refuses a second server on the *same* port, but a different port
pointed at the same home slips past that, and both servers would then happily
run the same agent twice.

So the home itself is claimed: a `server.lock` holding this process's pid.
`create_new` is atomic, so of two servers starting together exactly one wins,
and the loser is told who holds it. A pid left behind by a kill is recognised as
stale and taken over, so a crash does not lock the home forever -- only a live
process does.

The pid is written through the handle `create_new` returned, never by a separate
`fs::write` afterwards: the file becomes visible the moment it is created, so a
second server arriving in between would read an empty lock, call it litter, and
take a home that was already claimed. For the same reason an empty or unparsable
lock is *not* stale: it is a claim being written, and it is waited out (briefly)
rather than taken over.

## CLAIM_GRACE

The writer writes one short line through an already-open handle, so this is
orders of magnitude more than it needs; it exists so a genuinely empty file (a
disk that lies, a killed `create_new`) fails with a message instead of waiting
forever.

## ATTEMPTS

Each pass either wins `create_new` or learns something new about the file -- it
disappeared, it is being written, it names a live pid. The loop exists because
those are separate observations of a file other processes are changing: a pass
that finds the file gone goes round once more and races for it.

## InstanceLock::acquire

Someone was here first -- or a crash left its pid behind.

## InstanceLock::drop

Only our own claim is removed. A file that now names somebody else was taken over
after a `cancel` outlived this process's own lock -- deleting it would hand the
home to a third server.

Best effort: a kill leaves the file behind, which is exactly what the liveness
check exists for.

## claim

Returns `false` when the file was already there, which is not an error: the
caller is the one that knows what to do about the holder.

Written through *this* handle, and synced before it closes: the file is visible
to everybody from the instant `create_new` returns, so its contents must arrive
in the same breath.

## read_holder

`0` is not a pid any process has; it is what a half-written file reads as, which
is why it is not treated as a holder either.

Unreadable for any other reason: assume somebody is writing it. That is the
direction that cannot hand the home to a second server.

## wait_out_claim

The writer only has to put a few digits into an open handle, so a lock that stays
empty for `CLAIM_GRACE` is not being written by anybody -- but that is still not
grounds to take it over. The caller refuses instead, and the message names the
file so a human can decide.

## take_over

`rename` is the atomic part: of two servers that both decided the holder is dead,
exactly one moves the file, and the other sees it disappear. Neither writes over
the other's claim -- the following `create_new` is what decides the owner, and
only one process can win that.

Somebody else moved or removed it first: the caller's next attempt finds out who
owns the home now.

## alive

`tasklist` prints the matching line, or reports that nothing matched. Matching on
the pid itself is locale-independent.

Cannot tell: assume the holder is alive rather than let two servers into the same
home on a hunch.

## tests::a_pid_left_behind_by_a_dead_process_is_taken_over

A server that was killed never gets to clean up; its pid must not lock the home
forever.

## tests::an_empty_lock_is_not_taken_over

A lock that exists but has no pid in it is a claim being written, not litter.
Taking it over is how two servers end up sharing one home.

## tests::dropping_a_stale_claim_does_not_delete_the_new_owner

Releasing a claim removes only that claim: a lock that now names a different
process belongs to it, and dropping ours must not delete it.

## tests::only_one_of_many_starting_together_wins

The winners keep their claim until every thread is done: a claim that is released
immediately would let the next thread have the home, and the test would prove
nothing.
