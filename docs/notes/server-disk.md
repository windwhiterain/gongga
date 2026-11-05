# server-disk: the room a machine keeps back so a full disk can say so

A disk that fills up takes away the one thing the harness needs in order to report
it: somewhere to write. `monitor`'s report is a row in `gongga.db`, the tool that
noticed is a failing tool, and the turn that fixes it writes a transcript — all of
it in the home, on the same volume that has no room.

So the server holds room back:

```
<home>/config.toml    [server] disk_reserve_mb = 1024
GONGGA_DISK_RESERVE   the whole path of the file, for a test or a second machine
```

and spends it at the moment a tool fails on a disk that has none left.

## One file for the device, not one per home

Two servers on one machine fill the same disk, so two reserves would be two
answers to one question, and both of them would be spent by the same episode.
There is one file, on a machine-wide path (`%ProgramData%\gongga\gongga-disk-reserve`
on Windows, `/var/tmp/gongga/gongga-disk-reserve` elsewhere), and one claim on it.

What it buys is the volume it stands on. A home on another volume spends the file
and gets nothing for it; the config is per home, so a machine with two disks states
the same path in both homes and only the home on that volume is helped. That is
the honest limit of one device-wide file, and it is enough while the homes are on
one disk.

## The trigger is not the tool's words

The obvious test is on the failure's text, and it is the one that loses the
failure:

- a command's stderr goes through a pipe, a `2>/dev/null`, or a wrapper that prints
  its own sentence;
- the message is localized (`磁盘空间不足`, `拒绝访问`), and a list of English
  phrases matches nothing;
- `exit status 1` with empty output is what a killed writer looks like.

So the text is a hint and the measurement is the test: on a failed tool call, read
the room left on the volumes this server could write to (`GetDiskFreeSpaceExW`,
`statvfs` — one call each, no directory walk, microseconds) and compare it against
the reserve this machine declared. Less room left than the reserve holds is "this
volume is down to its last room", and either reading is enough to spend.

The volumes are every fixed drive plus the home's own, so a tool that failed on a
volume the home is not on is still seen. They are also what the report carries, so
a monitor starts with the map instead of spending its first call on a survey.

## The claim is the platform's atomic delete

`Reserve::claim` returns `Won` to exactly one caller in the machine and `Lost` to
everybody else, without a lock file, a coordinator, or a read before the delete.
That is what makes "two servers noticed at once" one wake and not two.

On unix that is `unlink`: the loser gets `ENOENT`.

On Windows it is **not** `DeleteFileW`. Measured, on this machine's NTFS: two
callers can both report a successful delete of one file, because the disposition is
applied to a handle that was already opened — the name is not what is decided. The
same race in the other direction returns `ERROR_ACCESS_DENIED` to the loser, which
a caller cannot tell from a real problem. So the claim is an exclusive open:
`CreateFileW(DELETE, share = 0, FILE_FLAG_DELETE_ON_CLOSE)`. The first caller gets
the handle, everybody else gets a sharing violation, and the file goes away with
the handle that won.

A sharing violation is also what an antivirus or an indexer causes for a moment, so
it is retried twice before the claim answers `Failed` — and `Failed` is logged, not
treated as "somebody else won". A silence that hides a full disk is the one outcome
worse than a double wake.

`scripts/monitor_selftest.sh` cannot see the race: it runs one server. The race is
`disk::tests::two_servers_share_one_claim_and_only_one_wins`, which runs sixteen
rounds of two threads on one file — and it is the test that found the Windows
answer, by failing once in four rounds against a delete-based claim.

## Arming is `create_new`, and the size is the machine's

At start-up the server arms the reserve: `create_new`, then `set_len`. `create_new`
is the same one-winner primitive in the other direction — the second server finds
the file and leaves it alone — and `set_len` takes real blocks, not a size in a
header. Measured: a 256 MiB reserve drops the volume's free space by exactly
268435456 bytes.

A writer killed between the create and the fill leaves a file that is short. The
yardstick is therefore the configured number and never the file's current size, and
the next writer grows it (`set_len` again) — a half reserve is worse than none,
because the number it claims is then a number nobody declared.

`disk_reserve_mb = 0` holds nothing back. The server says so once at start-up
instead of pretending to be protected.

## After the cleanup, the reserve comes back

The turn that spends the reserve is a monitor turn with `@skill: clean-disk`. Its
end is the fact that the disk has been dealt with:

- the report was accepted → `monitor_state` records the row it became
  (`disk.wake`);
- the monitor session has written past that row and is not in `error`;
- then the reserve is armed again, on the next two-second tick.

Before that, nothing re-arms it. A server that re-armed while the cleanup was still
running would give the next failed tool call a fresh way to wake a second monitor,
which is exactly what the claim exists to prevent.

Two things end the suppression early, and both are deliberate:

- **A restart.** A server that starts arms the reserve because it needs its own
  headroom and cannot know whether somebody else finished. So a restart in the
  middle of a cleanup ends the episode.
- **A disk that is still full.** `set_len` fails, the tick says so at `debug`, and
  the next tick asks again. No counter, no backoff, no retry budget — `AGENTS.md`
  forbids inventing one.

## What a failure costs, and what it does not

`tools::Registry::run` is where every tool call ends — a model's, a script's, the
command layer's — so it is where the check lives. A successful call costs nothing;
a failed one costs one free space reading per volume, and, on the day the disk is
actually full, the delete of the reserve and one `events` row (`kind =
'disk_space'`).

That row is the report's whole memory, the same way `sessions.status = 'error'` is
the error monitor's: it survives a crash between the failure and the handover, and
`monitor::sweep` reads it on the next tick. See `server-monitor.md`.

## Verified

| what | how |
| --- | --- |
| a reserve takes real space, and gives it back when spent | `disk::tests::the_space_a_reserve_holds_is_actually_taken` (64 MiB before/after) |
| one claim, one winner, sixteen rounds | `disk::tests::two_servers_share_one_claim_and_only_one_wins` |
| arming twice keeps the space and repairs a short file | `disk::tests::arming_twice_keeps_the_space_and_repairs_a_short_file` |
| the words a full disk is reported with are recognised | `disk::tests::the_usual_ways_a_full_disk_says_so_are_recognised` |
| the whole chain: failing tool → spent reserve → event → `@monitor/disk` → cleanup → armed again | `bash scripts/monitor_selftest.sh` |
