# The check names its own home

`evolve.sh swap` asks the successor "can you read this home's config?" before it cancels
anything, by running

    "$exe" server --home "$HOME_DIR" --check-config

`$exe` is the staged **bundle head**, and a head places its payload and retires the build
it replaces *before* it runs the program it was asked for (`docs/notes/bundle-main.md`,
"There is no read-only run"). It knows the program by its word: `server` replaces a
server. `--check-config` is parsed by the server afterwards, so the head cannot see it.

So the guard that exists to keep the home served *was the thing that stopped it*. Twice,
about fifteen minutes apart:

| time | what ran | what it left |
| --- | --- | --- |
| 14:10:43 | `launch 20261115-140713` → the launcher took the home | payload `dc9c…` placed in `~/.gongga`; retire asked `bin/gongga-cli cancel`; the server stopped at 14:10:48 with 5 turns running (GUI, desc, lichen-vm, lichen-vm/templates, @monitor/providers) |
| 14:25:36 | `swap 20261115-142346` (session `@monitor/providers`) | payload `ce8d…` placed at 14:25:43, server stopped 14:25:45 — and the swap's own turn, the successor launcher (pid 63124, heartbeat stops at 14:25:45) and the lease went with it |

The cancel is what makes it total: it is sent without `keep_alive`, so "every background
tree was let go" — the detached successor is in one of those trees. The home was left
with no server (port 9700 free, `server.lock` naming a dead pid) and nobody waiting to
take it, which is the one state the handover's ordering exists to make impossible.

`.dev/.config-check.log` is the witness: 0 bytes, still on disk, because the swap died
inside the check, before its own `rm -f`.

## The change

The check names its own home — the rule `bundle-main.md` already states for `--version`:

    check_home="$(cygpath -w "$RUN/check-home")"
    GONGGA_HOME="$check_home" "$exe" server --home "$HOME_DIR" --check-config

`--home` still names the real home, because that is the config being read. Only the head's
own install goes elsewhere. A Windows path, because the environment is read by a native
program.

Evidence: same binary, same live home, `--check-config` now answers
`config ok: C:\Users\17124\.gongga\ship\config\config.toml` (exit 0) and drops its payload
in `.dev/check-home/payload/ce8da75f33dc30d1`; `~/.gongga/payload` is unchanged (the same
two directories, same mtimes).

Cost: one payload set (~58 MB) under `.dev/check-home`, which the next build retires. `.dev`
is already this tree's scratch.

## Left alone

- `bundle::retire`'s rule is a safety invariant: an unmarked directory is somebody else's,
  and a `cancel` sent into it is not undoable. The head does not parse the program's argv
  for the same reason it retires before the program starts — a head that guessed flags
  would be a head that can be talked into stopping a stranger's server.
- A live server with pending turns replays them on the next start, so a home whose turns
  included this swap re-runs it. That is not a loop now: the replayed swap dies at its own
  guard (`successor pid 63124 is gone; not cancelling`) before it reaches the check.
