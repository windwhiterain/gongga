#!/usr/bin/env bash
# Self-evolution: replace the server that is running you.
#
# The pieces already exist, they were just never wired together:
#
#   * `cancel` stops the server but *keeps* the running turn as a pending task
#     (`commands.rs::cancel_and_preserve`), and the next start replays it;
#   * a detached job outlives the server that started it, which is what lets a
#     successor wait outside the process it is about to replace;
#   * `server.lock` is a pid file, so the home is handed over rather than
#     stolen, and a stale pid is recognised as litter.
#
# What was missing is one command that does the swap in the safe order: stage
# the new binary, start the successor *before* stopping the old server (so a
# bad build costs nothing), and only then cancel.
#
#   $PWD/.agents/skills/evolve/evolve.sh stage            # test, build, stage, write launcher
#   bash --detached: <the launcher it printed>
#   $PWD/.agents/skills/evolve/evolve.sh swap <stamp>     # verify the successor, then cancel
#   $PWD/.agents/skills/evolve/evolve.sh status           # what is staged, who is waiting
#   $PWD/.agents/skills/evolve/evolve.sh clean            # kill successors nobody is going to swap
#
# Only the newest stage may take the home. `stage` writes `.dev/evolve-current`,
# a launcher whose stamp is not in there leaves the waiting loop instead of
# racing for the port, and `swap` refuses a stamp that is not current. Without
# that pointer a waiter from an abandoned attempt keeps polling `tasklist` and
# `netstat` five times a second for as long as the old server lives -- forever,
# if nobody was going to swap it -- and when the port finally frees, the *older*
# binary can be the one that wins the race.
#
# After `swap` this turn is cancelled and preserved; the successor binds the
# port, picks the pending turn up and asks the model to continue. That is the
# whole loop: the harness replaces the process that is running it.
#
# See DESIGN.md §10 for why the order matters and what is still missing.
set -euo pipefail

# The repo root, found by walking up rather than by counting `../`: this script
# lives inside a skill directory, and a depth written into the code breaks the
# first time it moves.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
while [ "$ROOT" != "/" ] && [ ! -f "$ROOT/Cargo.toml" ]; do
  ROOT="$(dirname "$ROOT")"
done
[ -f "$ROOT/Cargo.toml" ] || { echo "evolve: no Cargo.toml above $0" >&2; exit 1; }
RUN="$ROOT/.dev"
# Which stage is allowed to take the home. Written by `stage`, read by every
# launcher and by `swap`.
CURRENT="$RUN/evolve-current"
HOME_DIR="${GONGGA_HOME:-$HOME/.gongga}"
BIND="${GONGGA_SERVER:-127.0.0.1:9700}"
PORT="${BIND##*:}"
# The successor is built here, not in target/. A server started from
# target/debug/gongga-server.exe cannot be relinked while it runs (Windows
# locks a running .exe: `os error 5`), and that server may be the very process
# this script exists to replace. A private target dir makes the build
# independent of what target/ is doing.
BUILD="$RUN/target"
mkdir -p "$RUN"

# The CLI has to be reachable from a plain bash call, exactly as the model
# would call it: the name on PATH first, then the build in this repo. Both
# spellings are asked for: `gongga` is what a home installed before the command
# was renamed, and a handover has to work across that rename.
CLI="$(command -v gongga-cli || command -v gongga-cli.exe || command -v gongga || command -v gongga.exe || true)"
if [ -z "$CLI" ] && [ -x "$ROOT/target/debug/gongga-cli.exe" ]; then
  CLI="$ROOT/target/debug/gongga-cli.exe"
elif [ -z "$CLI" ] && [ -x "$ROOT/target/debug/gongga.exe" ]; then
  CLI="$ROOT/target/debug/gongga.exe"
fi

die() { echo "evolve: $*" >&2; exit 1; }

# Free space on the volume holding this repo, and how full it is.
#
# Every swap says it: iterating on a Rust tree fills a disk, and the moment to
# know is before the build that needs the room, not after it fails. `df` is in
# Git Bash; a platform without it prints nothing rather than failing a handover.
disk_line() {
  local out
  out="$(df -h "$ROOT" 2>/dev/null | tail -n 1)" || return 0
  [ -n "$out" ] || return 0
  printf '%s  (free %s of %s, %s used)\n' "$1" \
    "$(printf '%s' "$out" | awk '{print $4}')" \
    "$(printf '%s' "$out" | awk '{print $2}')" \
    "$(printf '%s' "$out" | awk '{print $5}')"
}

# Reclaims the build tree. Only safe once the old server is gone: on Windows it
# is `target/debug/gongga-server.exe` that holds the lock, so this runs in the
# launcher, after the wait, not in `stage`.
clean_build_tree() {
  local before after
  before="$(df -h "$ROOT" 2>/dev/null | tail -n 1 | awk '{print $4}')"
  # CARGO_TARGET_DIR would send this somewhere else; the tree that grows is the
  # default one at $ROOT/target.
  if ! ( cd "$ROOT" && CARGO_TARGET_DIR= cargo clean ) >/dev/null 2>&1; then
    echo "evolve: cargo clean failed; keeping the tree and carrying on"
    return 0
  fi
  after="$(df -h "$ROOT" 2>/dev/null | tail -n 1 | awk '{print $4}')"
  echo "evolve: cargo clean done (free ${before:-?} -> ${after:-?})"
}

# The successor must not touch the home while the old server still owns it:
# `InstanceLock` refuses, and the UDP port is still bound. So it waits for both
# to go, then `exec`s.
#
# Measured on Windows: `exec` of a native .exe does NOT keep the pid. The bash
# launcher stays alive as the server's parent and the exe gets a fresh pid, so
# `server.lock` and this launcher's pid file disagree by design. `status` uses
# the parent link to recognise that case, and `adopt()`'s ancestor guard is what
# keeps `del_bash` from killing the server through its own parent.
# The template is filled in afterwards, not expanded while it is written.
#
# This used to be an unquoted heredoc, which meant every backtick, `$(...)` and
# `$var` in it ran *at generation time*: a comment that mentioned `tasklist`
# ran tasklist and pasted the whole process table into the launcher, which then
# died on the first line of it. Quoting the template removes the class, and the
# values are spliced in by `fill`, which escapes everything `sed` and the shell
# would otherwise read as syntax.
fill() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//&/\\&}"
  value="${value//|/\\|}"
  printf '%s' "$value"
}

write_launcher() {
  local stamp="$1" exe="$2" old_pid="$3" clean="${4:-0}"
  local argsfile="${5:-}"
  local path="$RUN/evolve-$stamp-launch.sh"
  local pidfile="$RUN/evolve-$stamp-launch.pid"
  local beatfile="$RUN/evolve-$stamp-launch.beat"
  sed \
    -e "s|@@STAMP@@|$(fill "$stamp")|g" \
    -e "s|@@EXE@@|$(fill "$exe")|g" \
    -e "s|@@OLD_PID@@|$(fill "$old_pid")|g" \
    -e "s|@@ARGSFILE@@|$(fill "$argsfile")|g" \
    -e "s|@@HOME_DIR@@|$(fill "$HOME_DIR")|g" \
    -e "s|@@PIDFILE@@|$(fill "$pidfile")|g" \
    -e "s|@@BEATFILE@@|$(fill "$beatfile")|g" \
    -e "s|@@ROOT@@|$(fill "$ROOT")|g" \
    -e "s|@@BIND@@|$(fill "$BIND")|g" \
    -e "s|@@CURRENT@@|$(fill "$CURRENT")|g" \
    -e "s|@@CLEAN@@|$(fill "$clean")|g" \
    > "$path" <<'LAUNCH'
#!/usr/bin/env bash
# Successor for @@STAMP@@: waits outside the old server, then takes the home.
set -u
# The pid alone is not proof of life: a stale pid file plus a recycled pid
# would look exactly like a waiting successor. The heartbeat is written by the
# loop below, so only a process actually sitting in that loop keeps it fresh.
# The pid file has to hold the *Windows* pid: every check downstream is a
# tasklist query, and under Git Bash $$ is an MSYS pid that tasklist never
# reports -- so a waiting successor would look dead and swap would refuse to
# cancel, which is the whole handover failing. /proc/$$/winpid is the same
# process in the other numbering; $$ is only a fallback for a bash that has no
# /proc (where pids already are what tasklist/ps see).
if [ -r /proc/$$/winpid ]; then
  cat /proc/$$/winpid > '@@PIDFILE@@'
else
  echo $$ > '@@PIDFILE@@'
fi
date +%s > '@@BEATFILE@@'
NEW='@@EXE@@'
OLD_PID='@@OLD_PID@@'
ARGS_FILE='@@ARGSFILE@@'
HOME_DIR='@@HOME_DIR@@'
FALLBACK_BIND='@@BIND@@'

# The old server's own argv, one token per line, read while it is still alive.
# The successor is the same server in a newer binary: same home, same address,
# same owner, same everything a flag said. `exec "$NEW"` with no argv used to
# produce a server nobody owned -- SPEC, Skill: evolve, is that a restart keeps
# the lifetime it had, "e.g. tui spawned server's lifetime bound to tui".
ARGS=()
if [ -s "$ARGS_FILE" ]; then
  while IFS= read -r line; do ARGS+=("$line"); done < "$ARGS_FILE"
fi
flag() { # the value after this flag, empty when it was not named
  local want="$1" i
  for ((i = 0; i < ${#ARGS[@]} - 1; i++)); do
    if [ "${ARGS[i]}" = "$want" ]; then
      printf '%s' "${ARGS[i + 1]}"
      return
    fi
  done
}
NEW_HOME="$(flag --home)"; NEW_HOME="${NEW_HOME:-$HOME_DIR}"
NEW_BIND="$(flag --bind)"; NEW_BIND="${NEW_BIND:-$FALLBACK_BIND}"
NEW_PARENT="$(flag --parent)"
PORT="${NEW_BIND##*:}"

# Nobody owns a home whose window is gone, and a server started with a dead
# --parent stops itself at once -- after resuming the turn it just picked up.
# Taking the home anyway would be a handover into a server that cannot live.
if [ -n "$NEW_PARENT" ] && ! tasklist //FI "PID eq $NEW_PARENT" //NH 2>/dev/null | grep -q "$NEW_PARENT"; then
  echo "evolve: the window that owned the old server (pid $NEW_PARENT) is gone; not taking the home"
  echo "evolve: start a window to get a server again, then stage a handover for it"
  exit 0
fi
# Wait for as long as it takes. A cap here would mean "the handover failed
# because the old server was slow", and AGENTS.md forbids turning a wait into a
# budget; the successor is detached, so waiting costs nobody anything. It says
# what it is waiting for instead, so a stuck handover is diagnosable.
tick=0
while :; do
  # Only the newest stage may take the home. A waiter whose attempt was
  # abandoned has to leave: left in the loop it polls tasklist and netstat five
  # times a second for as long as the old server lives, and when the port
  # finally frees it can beat the real successor to it -- with an older binary.
  # No pointer at all (an older stage, a hand-run script) means nothing
  # supersedes this one, so it waits as before.
  superseded=$(cat '@@CURRENT@@' 2>/dev/null || true)
  if [ -n "$superseded" ] && [ "$superseded" != '@@STAMP@@' ]; then
    echo "evolve: stage @@STAMP@@ was superseded by $superseded; leaving the queue"
    exit 0
  fi
  alive=0
  if [ -n "$OLD_PID" ] && tasklist //FI "PID eq $OLD_PID" //NH 2>/dev/null | grep -q "$OLD_PID"; then
    alive=1
  fi
  listening=$(netstat -ano 2>/dev/null | grep -c ":$PORT" || true)
  if [ "$alive" = 0 ] && [ "$listening" = 0 ]; then
    cd '@@ROOT@@'
    # Only now is the build tree unlocked: on Windows the old server *is*
    # target/debug/gongga-server.exe, so a clean before this point would fail.
    if [ '@@CLEAN@@' = "1" ]; then
      echo "evolve: cargo clean before taking the home"
      CARGO_TARGET_DIR= cargo clean || echo "evolve: cargo clean failed; carrying on"
    fi
    # The home is also where a new session starts, so the successor runs from it
    # exactly as the spawned server did (see cli::server::command).
    cd "$NEW_HOME" || { echo "evolve: no home at $NEW_HOME; not taking it" >&2; exit 1; }
    echo "evolve: taking the home as $NEW_HOME on $NEW_BIND${NEW_PARENT:+ for window $NEW_PARENT} (${#ARGS[@]} argument(s) carried over)"
    exec "$NEW" "${ARGS[@]}"
  fi
  date +%s > '@@BEATFILE@@'
  if [ $((tick % 60)) = 0 ]; then
    echo "evolve: waiting for the home (old pid ${OLD_PID:-none} alive=$alive, port $PORT still bound=$listening)"
  fi
  tick=$((tick + 1))
  # Fast while a handover is expected, slow once it is not. Each tick spawns
  # `tasklist` and `netstat`, and a waiter left behind by an abandoned attempt
  # polls forever: three of those at 5Hz exhausted MSYS's fork on this machine,
  # which then failed inside unrelated shells. Waiting is not urgent enough to
  # cost a process spawn every 200ms for minutes.
  if [ "$tick" -le 25 ]; then sleep 0.2; else sleep 1; fi
done
LAUNCH
  chmod +x "$path"
  # A launcher that does not parse is worse than none: `swap` would cancel the
  # old server for a successor that cannot start.
  bash -n "$path" || die "generated launcher $path does not parse"
  echo "$path"
}

# The parent pid of a live process, or nothing. PowerShell because it is the one
# thing on this platform that answers without extra tooling.
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" \
    2>/dev/null | tr -d '\r' | grep -E '^[0-9]+$' || true
}

# The command line of a running process, as one string; failure when it cannot
# be read at all.
server_cmdline() { # pid
  local pid="$1"
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      # `wmic` is deprecated but present wherever Git Bash is; `//value` drops
      # the column header, and every line still carries the `CommandLine=` key.
      wmic process where "ProcessId=$pid" get CommandLine //value 2>/dev/null \
        | sed -n 's/^CommandLine=//p' | tr -d '\r' | sed '/^$/d'
      ;;
    *)
      [ -r "/proc/$pid/cmdline" ] || return 1
      tr '\0' '\n' < "/proc/$pid/cmdline" | sed '/^$/d'
      ;;
  esac
}

# The argv of a running process, one token per line, argv[0] included.
#
# Split the way a command line is split -- honouring the double quotes a value
# with a space arrives in -- so a path stays one token.
server_tokens() { # pid
  server_cmdline "$1" | awk '
    BEGIN { out = ""; inq = 0 }
    {
      s = $0
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\"") { inq = !inq; continue }
        if (c == " " && !inq) { if (out != "") { print out; out = "" } continue }
        out = out c
      }
      if (out != "") { print out; out = "" }
    }'
}

# The flags that decide how a server is reachable and who owns it, read from the
# server that is running now: one "flag<TAB>value" line each.
#
# SPEC, Skill: evolve: "restart server should still keep its lifetime, e.g. tui
# spawned server's lifetime bound to tui". The window that owns the old server is
# named in its `--parent`, so the successor has to name the same window: an
# `exec "$NEW"` with no argv produces a server nobody owns, which is a lifetime
# that silently changed hands to nobody.
running_flags() { # pid
  server_tokens "$1" | awk '
    tok { print flag "\t" $0; tok = 0; next }
    $0 == "--home" || $0 == "--bind" || $0 == "--parent" { flag = $0; tok = 1 }'
}

# One flag of a running server, empty when it was not named.
flag_of() { # pid, flag
  running_flags "$1" | awk -F '\t' -v want="$2" '$1 == want { print $2; exit }'
}

running_pid() {
  local pid
  pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || true)"
  [ -n "$pid" ] || return 1
  tasklist //FI "PID eq $pid" //NH 2>/dev/null | grep -q "$pid" || return 1
  echo "$pid"
}

cmd_stage() {
  local all=0 clean=0
  for flag in "$@"; do
    case "$flag" in
      --all) all=1 ;;
      --clean) clean=1 ;;
    esac
  done

  disk_line "disk:"

  echo "== tests =="
  # Test binaries are named after the crate, not after the shipped exe, so this
  # does not need to relink a running server or client.
  cargo test --workspace 2>&1 | tail -n 25

  echo "== build =="
  if [ "$all" = 1 ]; then
    CARGO_TARGET_DIR="$BUILD" cargo build --workspace
  else
    # Server-only by default: the TUI may be running from
    # target/debug/gongga-tui.exe and a workspace build would have to relink it.
    # The protocol is unchanged, so the old client still speaks to the new
    # server.
    CARGO_TARGET_DIR="$BUILD" cargo build -p gongga-server
  fi

  local stamp exe old_pid launcher
  stamp="$(date +%Y%m%d-%H%M%S)"
  exe="$RUN/evolve-$stamp-server.exe"
  cp "$BUILD/debug/gongga-server.exe" "$exe" || die "copying the server failed"
  old_pid="$(running_pid || true)"
  [ -n "$old_pid" ] || echo "note: no live server holds $HOME_DIR/server.lock"
  # The successor has to be the old server over again: its argv is recorded now,
  # while it can still be read, and handed to the new binary verbatim.
  local argsfile="$RUN/evolve-$stamp-args"
  : > "$argsfile"
  local old_home="$HOME_DIR" old_bind="$BIND" old_parent=""
  if [ -n "$old_pid" ]; then
    # Read now, while it can still answer: after the cancel there is nothing left
    # to ask. Unreadable is a refusal rather than a guess, because the guess is
    # what produced an unowned server in the first place.
    if [ -z "$(server_cmdline "$old_pid" 2>/dev/null)" ]; then
      die "cannot read pid $old_pid's command line, so what the successor should be is \
unknown; refusing to stage a handover that would produce a server nobody owns"
    fi
    server_tokens "$old_pid" | tail -n +2 > "$argsfile"
    old_home="$(flag_of "$old_pid" --home)"; [ -n "$old_home" ] || old_home="$HOME_DIR"
    old_bind="$(flag_of "$old_pid" --bind)"; [ -n "$old_bind" ] || old_bind="$BIND"
    old_parent="$(flag_of "$old_pid" --parent)"
    echo "old server: home $old_home, bind $old_bind, owner ${old_parent:-none}"
  fi
  launcher="$(write_launcher "$stamp" "$exe" "$old_pid" "$clean" "$argsfile")"
  # The previous attempt is abandoned from here on: its launcher sees itself
  # superseded and leaves the waiting loop by itself (see the loop below).
  echo "$stamp" > "$CURRENT"

  {
    echo "stamp=$stamp"
    echo "server=$exe"
    echo "old_pid=${old_pid:-none}"
    echo "old_home=$old_home"
    echo "old_bind=$old_bind"
    echo "old_owner=${old_parent:-none}"
    echo "old_args=$argsfile"
    echo "launcher=$launcher"
  } > "$RUN/evolve-$stamp.txt"
  cat "$RUN/evolve-$stamp.txt"
  echo
  echo "next: launch detached, then swap"
  echo "  bash --detached: $launcher"
  echo "  $PWD/.agents/skills/evolve/evolve.sh swap $stamp"
  echo
  if [ "$clean" = 1 ]; then
    echo "armed: the successor runs 'cargo clean' after the old server is gone, before it binds"
  fi
  echo "an earlier attempt still waiting will leave on its own; 'clean' says who is left"
}

cmd_swap() {
  local stamp="${1:?usage: evolve.sh swap <stamp>}"
  local exe="$RUN/evolve-$stamp-server.exe"
  [ -f "$exe" ] || die "no staged server for $stamp; run stage first"

  # Only the newest stage may take the home. An older stamp still has its
  # files, but its launcher has left the queue (it saw the pointer move), and
  # cancelling for it would hand the home to nothing.
  local current
  current="$(cat "$CURRENT" 2>/dev/null || true)"
  if [ -z "$current" ]; then
    echo "note: no $CURRENT; not checking whether $stamp is the newest stage"
  elif [ "$stamp" != "$current" ]; then
    die "stage $stamp is not the current handover target (current: $current); \
its launcher has already left the queue -- run stage again if you meant the newest build"
  fi

  # A successor that is not actually waiting must stop the handover: cancelling
  # without one would leave the home with no server and nothing to resume this
  # turn with. The launcher records its pid before it waits, so this is a real
  # check rather than a guess.
  local launch_pid
  launch_pid="$(cat "$RUN/evolve-$stamp-launch.pid" 2>/dev/null || true)"
  [ -n "$launch_pid" ] || die "successor was never launched; not cancelling"
  tasklist //FI "PID eq $launch_pid" //NH 2>/dev/null | grep -q "$launch_pid" \
    || die "successor pid $launch_pid is gone; not cancelling \
(if the pid looks like a small MSYS number, the launcher was written before \
/proc/\$\$/winpid was recorded: re-run stage and launch the new one)"
  # A live pid is not necessarily *our* launcher: pids get reused, and a pid
  # file left by an earlier attempt would then look like a successor waiting.
  # Only the waiting loop writes the heartbeat, so a fresh one means something
  # really is sitting there ready to take the home. One beat per 0.2s, so 10s
  # of slack is room for a loaded machine, not a budget: too tight only means
  # "refused, retried", too loose means cancelling after the waiter died.
  local beat now
  beat="$(cat "$RUN/evolve-$stamp-launch.beat" 2>/dev/null || echo 0)"
  now="$(date +%s)"
  [ $((now - beat)) -le 10 ] \
    || die "successor pid $launch_pid has no fresh heartbeat; not cancelling"

  local pid
  pid="$(running_pid || true)"
  [ -n "$pid" ] || die "no live server on $HOME_DIR; refusing to cancel"
  if [ -z "$CLI" ] || [ ! -x "$CLI" ]; then
    die "no gongga CLI to send cancel with; is one on PATH?"
  fi
  disk_line "disk before:"
  echo "old server pid $pid; successor $exe (pid $launch_pid) is waiting"
  echo "cancelling: this turn is cancelled and preserved, the successor resumes it"
  "$CLI" cancel
}

cmd_status() {
  disk_line "disk:"
  echo "current: $(cat "$CURRENT" 2>/dev/null || echo none)"
  local armed_launcher
  armed_launcher="$RUN/evolve-$(cat "$CURRENT" 2>/dev/null)-launch.sh"
  if grep -q 'cargo clean' "$armed_launcher" 2>/dev/null; then
    echo "armed: the current successor runs 'cargo clean' before it binds"
  fi
  local server_pid
  server_pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || echo none)"
  echo "server.lock: $server_pid"
  netstat -ano 2>/dev/null | grep ":$PORT" || echo "port $PORT: not bound"
  echo "staged:"
  ls -t "$RUN"/evolve-*.txt 2>/dev/null | head -n 3
  echo "successors:"
  local current f p stamp mark
  current="$(cat "$CURRENT" 2>/dev/null || true)"
  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    p="$(cat "$f")"
    stamp="$(basename "$f")"
    stamp="${stamp#evolve-}"
    stamp="${stamp%-launch.pid}"
    tasklist //FI "PID eq $p" //NH 2>/dev/null | grep -q "$p" || continue
    # Two shapes, because Windows does not honour `exec` for a native exe: either
    # the pid was kept (unix) or the launcher became the server's parent.
    if [ "$p" = "$server_pid" ] || parent_of "$server_pid" | grep -qx "$p"; then
      echo "  $stamp: pid $p is running the server"
      continue
    fi
    mark=""
    if [ "$stamp" = "$current" ]; then
      mark=" (current)"
    else
      mark=" (superseded; 'clean' ends it)"
    fi
    echo "  $stamp: pid $p waiting$mark"
  done
}

# Ends successors nobody is going to swap, and optionally the files they left.
#
# The reason this exists rather than being automatic on every `stage`: a waiter
# that predates the supersede check in the launcher loop has no way out, and a
# machine that has been evolving for a while has a pile of them. Each one is
# five `tasklist`/`netstat` rounds a second, forever, and every later server
# adopts it, so it also shows up in `list_bash` as a job that cannot be used.
cmd_clean() {
  local artifacts=0
  [ "${1:-}" = "--artifacts" ] && artifacts=1
  local current f p stamp server_pid live_stamp removed
  local killed=0 alive=0
  local -a keep=()
  current="$(cat "$CURRENT" 2>/dev/null || true)"
  # The pid file of the stage that `exec`ed into the server holds the *server's*
  # pid, and that stage is usually not the current one any more -- a later
  # `stage` moves the pointer. Killing it would kill the very process this
  # command is running under, so the live server is never a candidate.
  server_pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || true)"
  live_stamp=""
  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    [ -n "$server_pid" ] || break
    [ "$(cat "$f" 2>/dev/null || true)" = "$server_pid" ] || continue
    live_stamp="${f##*/evolve-}"
    live_stamp="${live_stamp%-launch.pid}"
  done

  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    p="$(cat "$f" 2>/dev/null || true)"
    stamp="$(basename "$f")"
    stamp="${stamp#evolve-}"
    stamp="${stamp%-launch.pid}"
    [ -n "$p" ] || continue
    tasklist //FI "PID eq $p" //NH 2>/dev/null | grep -q "$p" || continue
    if [ -n "$server_pid" ] && [ "$p" = "$server_pid" ]; then
      echo "  keep    $stamp (pid $p is the running server)"
      continue
    fi
    if [ "$stamp" = "$current" ]; then
      echo "  keep    $stamp (pid $p, current)"
      continue
    fi
    if taskkill //PID "$p" //T //F >/dev/null 2>&1; then
      echo "  killed  $stamp (pid $p, superseded)"
      killed=$((killed + 1))
    else
      echo "  left    $stamp (pid $p); could not kill it"
      alive=$((alive + 1))
    fi
  done
  echo "waiters: $killed killed, $alive left running"

  if [ "$artifacts" = 0 ]; then
    echo "(pass --artifacts to delete the staged files of every stamp but the current one)"
    return 0
  fi

  # The staged `-server.exe` is ~18MB, nothing ever rewrites it, and a stamp
  # that is not current can no longer be swapped -- so the copy buys nothing.
  # The current stamp keeps everything: it is the one that may still be
  # launched. So does the stamp the running server came from, whose exe is the
  # image of a live process.
  #
  # One `find -delete` rather than a loop of `rm`: the shells this runs in can
  # have a delete wrapper on PATH that blocks per file, and a cleanup command is
  # the last place that should hang. One process, one pass.
  if [ -z "$current" ]; then
    echo "no evolve-current, so nothing says which stamp is live; not deleting anything"
    return 0
  fi
  # The stamp is followed by a dash in most artifacts and by a dot in the `.txt`
  # report, so both spellings have to be kept.
  keep=( -name "evolve-$current-*" -o -name "evolve-$current.*" )
  if [ -n "$live_stamp" ] && [ "$live_stamp" != "$current" ]; then
    keep+=( -o -name "evolve-$live_stamp-*" -o -name "evolve-$live_stamp.*" )
  fi
  removed=$(find "$RUN" -maxdepth 1 -type f \
    \( -name 'evolve-*-launch.sh' -o -name 'evolve-*-launch.pid' \
       -o -name 'evolve-*-launch.beat' -o -name 'evolve-*-server.exe' \
       -o -name 'evolve-*.txt' \) \
    ! \( "${keep[@]}" \) -print -delete 2>/dev/null | wc -l)
  echo "artifacts: $removed file(s) removed"
}

case "${1:-help}" in
  stage)  shift; cmd_stage "$@" ;;
  swap)   shift; cmd_swap "$@" ;;
  status) cmd_status ;;
  clean)  shift; cmd_clean "$@" ;;
  *)      awk 'NR > 1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
          exit 1 ;;
esac
