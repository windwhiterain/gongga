# lock/src/lib.rs — one home, one owner

## Why a lock at all

Two processes sharing a home is a state nothing else here has an answer for.

For a **server** home they share the SQLite file, so each would resume the same
pending turn, compress the same transcript, and drive the same session at the
same time. Binding the UDP port already refuses a second server on the *same*
port, but a different port pointed at the same home slips past that, and both
servers would then happily run the same agent twice.

For a **client** home they share `roster/` and `events/`: two clients append to
the same journal, each `seek(End)` then `write_all`, so records interleave and a
length check throws the damaged one away. The visible result is a window whose
"scroll up" boundary is wrong -- a repeated or missing row -- and no error
anywhere.

So the home itself is claimed: a file holding this process's pid
(`server.lock`, `client.lock`).

## How the claim works

`create_new` is atomic, so of two processes starting together exactly one wins,
and the loser is told who holds it. A pid left behind by a kill is recognised as
stale and taken over, so a crash does not lock the home forever -- only a live
process does.

The pid is written through the handle `create_new` returned, never by a separate
`fs::write` afterwards: the file becomes visible the moment it is created, so a
second process arriving in between would read an empty lock, call it litter, and
take a home that was already claimed. For the same reason an empty or unparsable
lock is *not* stale: it is a claim being written, and it is waited out (briefly)
rather than taken over.

`CLAIM_GRACE` is orders of magnitude more than writing one short line needs; it
exists so a genuinely empty file (a disk that lies, a killed `create_new`) fails
with a message instead of waiting forever.

`ATTEMPTS` exists because each pass either wins `create_new` or learns something
new about the file -- it disappeared, it is being written, it names a live pid.
Those are separate observations of a file other processes are changing: a pass
that finds the file gone goes round once more and races for it.

## The pieces

- **`claim`** returns `false` when the file was already there, which is not an
  error: the caller is the one that knows what to do about the holder.
- **`read_holder`**: `0` is not a pid any process has; it is what a half-written
  file reads as, which is why it is not treated as a holder either. Unreadable
  for any other reason: assume somebody is writing it. That is the direction
  that cannot hand the home to a second owner.
- **`wait_out_claim`**: a lock that stays empty for `CLAIM_GRACE` is not being
  written by anybody -- but that is still not grounds to take it over. The
  caller refuses instead, and the message names the file so a human can decide.
- **`take_over`**: `rename` is the atomic part. Of two processes that both
  decided the holder is dead, exactly one moves the file, and the other sees it
  disappear. Neither writes over the other's claim -- the following `create_new`
  is what decides the owner, and only one process can win that.
- **`alive`**: `tasklist` prints the matching line, or reports that nothing
  matched; matching on the pid itself is locale-independent. Cannot tell: assume
  the holder is alive rather than let two owners into the same home on a hunch.
- **`Drop`**: only our own claim is removed. A file that now names somebody else
  was taken over after a `cancel` outlived this process's own lock -- deleting it
  would hand the home to a third process. Best effort: a kill leaves the file
  behind, which is exactly what the liveness check exists for.

## Who takes it, and what it says

`HomeLock::acquire(home, file, owner)` creates the home if it does not exist, so a
first run has somewhere to put the lock. `owner` is the words an occupied home is
reported in:

    another gongga client is already using this home (pid 38936, home
    C:/.../.gongga-client); stop it first, pass another directory, or delete
    C:/.../.gongga-client\client.lock if you are sure that process is gone

`gongga-server` takes `server.lock`; `gongga-client` takes `client.lock` in its
own client home (`--client-home` / `GONGGA_CLIENT_HOME` / `~/.gongga-client`).
A second window therefore needs its own client home -- `gongga-avalonia
--instance <name>` maps to `<client-home>/<name>` for exactly this reason.

## What a lock does not cover

`client.toml` -- sticky session, shown sessions, block size, the remembered
address -- lives in the **server** home (`~/.gongga/client.toml`), not the client
home. Two clients on one machine still share it and can overwrite each other's
setting. That is a value, not a record: nothing is corrupted, the last writer
wins. Isolating it would mean moving the file, which is a migration rather than
a lock.

`record_dir` (the `<home>/server.cwd` file a handover reads) is in
[`server-instance.md`](server-instance.md).
