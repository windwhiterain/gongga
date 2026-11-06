# server-disk: the room left, read after every tool call

A disk that fills up takes away the one thing a monitor needs to report it: room to
write. It also does something worse to a design that watches for a *failure*: it
lets the harness keep working. A volume with a hundred megabytes left serves `cat`,
`ls`, `grep` and a shell that only reads; the first command that fails is whichever
one happens to write something big, and on a server that mostly reads there may be
none before the machine is unusable.

That was measured on this machine on 2026-11-05: the disk reached zero free, `cargo
build` failed with `os error 112`, and the monitor that was supposed to be woken by
exactly that never was — no `disk_space` event, no log line. So the reading is not
attached to a failure at all.

## What it reads, and how often

`tools::invoke` is where every tool call ends — a model's call, a script's, the
command layer's, a call the tunnel detached. After each one, whether it worked or
not, the server reads the room left on the volumes it could write to:

| platform | call |
| --- | --- |
| Windows | `GetDiskFreeSpaceExW` per fixed drive, plus the home's own volume |
| unix | `statvfs` of the home's volume |

One call per volume, no directory walk, no file written: microseconds against the
milliseconds a tool call costs anyway. Nothing is held back on disk — the first
version of this reserved a gigabyte of real space and spent it as the report's
room, and that design was wrong twice over: it could not arm on a disk that was
already full, and it only looked when a tool failed.

The smallest reading decides. A command writes wherever its own words say, so the
volume that is out of room is whichever answers lowest, and the report carries all
of them so a monitor starts with the map instead of its own survey.

## When it says so: one episode, not one call

The threshold is `[server] disk_low_mb` (default 1024, `0` turns the reading off),
or `GONGGA_DISK_LOW_MB` for a run. It is a threshold and nothing else: no budget,
no reserve, nothing written to compare against.

Read on every call, compared on every call, **reported once**: the server keeps one
bit of state in memory — "this disk has already been reported as low" — set when the
reading falls below the line and cleared when a reading comes back above it. So

- a disk that stays low wakes nothing twice, however many tool calls it serves;
- a disk that recovers and falls again is news again;
- a restart reads the disk afresh and says so once, which is what a restart should
  do.

Nothing durable is involved, which is the difference from the error kind: that one
has `sessions.status` and a transcript to point at, and this one has a number.

## The event is the report's whole memory

When the edge is crossed the server writes one row, `events(kind = 'disk_space')`,
carrying the session, the tool, whether that tool succeeded, the threshold, and the
volumes as they stood. From there it is exactly the error kind's path:
`monitor::sweep` reads the row's id against `monitor_state('disk.event')`, and hands
one report per row to `@monitor/disk` through `commands::deliver`.

That is why a crash between the reading and the handover loses nothing: the row is
the fact, and the sweep is stateless but for the cursor.

`monitor_state` also holds `disk.attempts` and `disk.wake`. The disk monitor has no
failing session row of its own to hang a retry on, so its cursor and its attempt
count live here: while its session is in `error`, the same event is handed over
again on the next model, and a disk whose first choice of model is refused is still
cleaned. See `server-monitor.md`.

## What a report says

The report is written so a monitor can start work without reading anything first:
the room left per volume, the threshold, which session's tool call was running and
whether it succeeded, the build, the home, the pid, and the read-back command. The
standing map — where a home keeps its database and its log, how to tell a provider's
refusal from a harness bug — comes from `@skill: clean-disk`, which is an index
entry in the prompt's prefix rather than text in the message.

## A peer's disk is read by the peer

A session whose shells are on a peer writes there, so that machine's disk is the one
that can fill up. Its room is asked for on the runner, not here:

| where | what |
| --- | --- |
| `remote/protocol.rs` | `Op::Space { path }` |
| `remote/runner.rs` | answered on the peer, with the same `disk::volumes()`/`free_space()` this machine uses on itself |
| `remote/transport.rs` | `Runner::space`, one request on the connection the session already keeps |
| `remote/files.rs` | `Reader::space`, so a tool call costs one round trip and touches no shell |
| `disk.rs` | one episode per **machine**, and the row carries `machine` |

The scheme is the same point values as locally: a request multiplexed by id cannot
overtake a shell or swallow output a tool still owes, and the answer describes the
machine that has to live with the answer.

## One monitor, and the machine it is told about

`@monitor(disk)` is always **this machine's** session: its shells are here, and the
cleaner it has is a file here. A report about a peer therefore names the machine and
references `@skill(remote)` beside `@skill(clean-disk)`, so the monitor reaches that
machine (`remote run <machine> …`) instead of cleaning the wrong one. One name, one
report, and the machine is a fact in it.

## Two boundaries

- **A home and the volumes above it.** The drive letters are named as they are, and
  the threshold is one number for all of them: a machine that wants one disk watched
  more closely than another is asking for two numbers, which `AGENTS.md` is right to
  refuse until somebody has a reason.
- **A peer nobody has called yet.** Its reading arrives with a tool call that ran
  there; a peer whose disk fills up while no session talks to it is not watched, and
  that is the same line the local reading draws -- nothing polls.

## Verified

| what | how |
| --- | --- |
| the room is read from the volume and moves when a big file does | `disk::tests::the_room_left_is_read_from_the_volume_itself` |
| one episode per fall, and a new one after recovering | `disk::tests::a_low_disk_is_reported_once_and_again_after_recovering` |
| the smallest volume decides | `disk::tests::the_volume_with_the_least_room_decides` |
| a successful tool call on a low disk is enough to wake the monitor, with `@skill: clean-disk` in its index, on the second model | `bash scripts/disk_selftest.sh` |

The e2e script states a threshold no disk is above (`[server] disk_low_mb` in the
test home's own config), because a test cannot fill a disk and the chain from a
reading to a turn is what it is there for. "An ordinary disk says nothing" is the
unit test's half, where both sides of the line can be chosen.
