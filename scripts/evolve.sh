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
#   scripts/evolve.sh stage            # test, build, stage, write launcher
#   bash --detached: <the launcher it printed>
#   scripts/evolve.sh swap <stamp>     # verify the successor, then cancel
#   scripts/evolve.sh status           # what is staged, who is waiting
#   scripts/evolve.sh clean            # kill successors nobody is going to swap
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

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
# would call it: prefer the one the harness exported, then the build here.
if [ -n "${GONGGA_CLI:-}" ] && [ -x "${GONGGA_CLI}" ]; then
  CLI="$GONGGA_CLI"
elif [ -x "$ROOT/target/debug/gongga.exe" ]; then
  CLI="$ROOT/target/debug/gongga.exe"
else
  CLI="$(command -v gongga.exe || command -v gongga || true)"
fi

die() { echo "evolve: $*" >&2; exit 1; }

# The successor must not touch the home while the old server still owns it:
# `InstanceLock` refuses, and the UDP port is still bound. So it waits for both
# to go, then execs -- exec, not spawn, because that keeps the pid the old
# server recorded for this job, and `adopt()` drops rows naming its own pid.
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
  local stamp="$1" exe="$2" old_pid="$3"
  local path="$RUN/evolve-$stamp-launch.sh"
  local pidfile="$RUN/evolve-$stamp-launch.pid"
  local beatfile="$RUN/evolve-$stamp-launch.beat"
  sed \
    -e "s|@@STAMP@@|$(fill "$stamp")|g" \
    -e "s|@@EXE@@|$(fill "$exe")|g" \
    -e "s|@@OLD_PID@@|$(fill "$old_pid")|g" \
    -e "s|@@PIDFILE@@|$(fill "$pidfile")|g" \
    -e "s|@@BEATFILE@@|$(fill "$beatfile")|g" \
    -e "s|@@ROOT@@|$(fill "$ROOT")|g" \
    -e "s|@@PORT@@|$(fill "$PORT")|g" \
    -e "s|@@CURRENT@@|$(fill "$CURRENT")|g" \
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
  listening=$(netstat -ano 2>/dev/null | grep -c ':@@PORT@@' || true)
  if [ "$alive" = 0 ] && [ "$listening" = 0 ]; then
    cd '@@ROOT@@'
    exec "$NEW"
  fi
  date +%s > '@@BEATFILE@@'
  if [ $((tick % 150)) = 0 ]; then
    echo "evolve: waiting for the home (old pid ${OLD_PID:-none} alive=$alive, port @@PORT@@ still bound=$listening)"
  fi
  tick=$((tick + 1))
  sleep 0.2
done
LAUNCH
  chmod +x "$path"
  # A launcher that does not parse is worse than none: `swap` would cancel the
  # old server for a successor that cannot start.
  bash -n "$path" || die "generated launcher $path does not parse"
  echo "$path"
}

running_pid() {
  local pid
  pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || true)"
  [ -n "$pid" ] || return 1
  tasklist //FI "PID eq $pid" //NH 2>/dev/null | grep -q "$pid" || return 1
  echo "$pid"
}

cmd_stage() {
  local all=0
  [ "${1:-}" = "--all" ] && all=1

  echo "== tests =="
  # Test binaries are named after the crate, not after the shipped exe, so this
  # does not need to relink a running server or client.
  cargo test --workspace 2>&1 | tail -n 25

  echo "== build =="
  if [ "$all" = 1 ]; then
    CARGO_TARGET_DIR="$BUILD" cargo build --workspace
  else
    # Server-only by default: the TUI may be running from
    # target/debug/gongga.exe and a workspace build would have to relink it.
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
  launcher="$(write_launcher "$stamp" "$exe" "$old_pid")"
  # The previous attempt is abandoned from here on: its launcher sees itself
  # superseded and leaves the waiting loop by itself (see the loop below).
  echo "$stamp" > "$CURRENT"

  {
    echo "stamp=$stamp"
    echo "server=$exe"
    echo "old_pid=${old_pid:-none}"
    echo "launcher=$launcher"
  } > "$RUN/evolve-$stamp.txt"
  cat "$RUN/evolve-$stamp.txt"
  echo
  echo "next: launch detached, then swap"
  echo "  bash --detached: $launcher"
  echo "  scripts/evolve.sh swap $stamp"
  echo
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
    die "no gongga CLI to send cancel with; set GONGGA_CLI"
  fi
  echo "old server pid $pid; successor $exe (pid $launch_pid) is waiting"
  echo "cancelling: this turn is cancelled and preserved, the successor resumes it"
  "$CLI" cancel
}

cmd_status() {
  echo "current: $(cat "$CURRENT" 2>/dev/null || echo none)"
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
    if [ "$p" = "$server_pid" ]; then
      # The launcher `exec`ed: this pid is the server, not a waiter.
      echo "  $stamp: pid $p is the running server"
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
