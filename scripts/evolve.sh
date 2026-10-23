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
#
# After `swap` this turn is cancelled and preserved; the successor binds the
# port, picks the pending turn up and asks the model to continue. That is the
# whole loop: the harness replaces the process that is running it.
#
# See DESIGN.md §10 for why the order matters and what is still missing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN="$ROOT/.dev"
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
write_launcher() {
  local stamp="$1" exe="$2" old_pid="$3"
  local path="$RUN/evolve-$stamp-launch.sh"
  local pidfile="$RUN/evolve-$stamp-launch.pid"
  local beatfile="$RUN/evolve-$stamp-launch.beat"
  cat > "$path" <<LAUNCH
#!/usr/bin/env bash
# Successor for $stamp: waits outside the old server, then takes the home.
set -u
# The pid alone is not proof of life: a stale pid file plus a recycled pid
# would look exactly like a waiting successor. The heartbeat is written by the
# loop below, so only a process actually sitting in that loop keeps it fresh.
# The pid file has to hold the *Windows* pid: every check downstream is a
# `tasklist` query, and under Git Bash `$$` is an MSYS pid that `tasklist`
# never reports -- so a waiting successor would look dead and `swap` would
# refuse to cancel, which is the whole handover failing. `/proc/$$/winpid` is
# the same process in the other numbering; `$$` is only a fallback for a bash
# that has no /proc (where pids already are what `tasklist`/`ps` see).
if [ -r /proc/\$\$/winpid ]; then
  cat /proc/\$\$/winpid > '$pidfile'
else
  echo \$\$ > '$pidfile'
fi
date +%s > '$beatfile'
NEW='$exe'
OLD_PID='$old_pid'
# Wait for as long as it takes. A cap here would mean "the handover failed
# because the old server was slow", and AGENTS.md forbids turning a wait into a
# budget; the successor is detached, so waiting costs nobody anything. It says
# what it is waiting for instead, so a stuck handover is diagnosable.
tick=0
while :; do
  alive=0
  if [ -n "\$OLD_PID" ] && tasklist //FI "PID eq \$OLD_PID" //NH 2>/dev/null | grep -q "\$OLD_PID"; then
    alive=1
  fi
  listening=\$(netstat -ano 2>/dev/null | grep -c ':$PORT' || true)
  if [ "\$alive" = 0 ] && [ "\$listening" = 0 ]; then
    cd '$ROOT'
    exec "\$NEW"
  fi
  date +%s > '$beatfile'
  if [ \$((tick % 150)) = 0 ]; then
    echo "evolve: waiting for the home (old pid \${OLD_PID:-none} alive=\$alive, port $PORT still bound=\$listening)"
  fi
  tick=\$((tick + 1))
  sleep 0.2
done
LAUNCH
  chmod +x "$path"
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
}

cmd_swap() {
  local stamp="${1:?usage: evolve.sh swap <stamp>}"
  local exe="$RUN/evolve-$stamp-server.exe"
  [ -f "$exe" ] || die "no staged server for $stamp; run stage first"

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
`/proc/\$\$/winpid` was recorded: re-run `stage` and launch the new one)"
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
  echo "server.lock: $(cat "$HOME_DIR/server.lock" 2>/dev/null || echo none)"
  netstat -ano 2>/dev/null | grep ":$PORT" || echo "port $PORT: not bound"
  echo "staged:"
  ls -t "$RUN"/evolve-*.txt 2>/dev/null | head -n 3
  echo "waiting successors:"
  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    local p; p="$(cat "$f")"
    if tasklist //FI "PID eq $p" //NH 2>/dev/null | grep -q "$p"; then
      echo "  $(basename "$f" .pid): pid $p alive"
    fi
  done
}

case "${1:-help}" in
  stage)  shift; cmd_stage "$@" ;;
  swap)   shift; cmd_swap "$@" ;;
  status) cmd_status ;;
  *)      sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
