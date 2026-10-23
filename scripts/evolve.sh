#!/usr/bin/env bash
# Self-evolution cycle for the harness.
#
# The pieces already exist, they were just never wired together:
#
#   * `cancel` stops the server but *keeps* the running turn as a pending task
#     (commands.rs: cancel_and_preserve), and the next start replays it;
#   * a detached job outlives the server that started it, which is what lets a
#     successor wait outside the process it is about to replace;
#   * `server.lock` is a pid file, so the home is handed over rather than
#     stolen, and a stale pid is recognised as litter.
#
# What was missing is one command that does the swap in the safe order:
# stage the new binary, start the successor *before* stopping the old server
# (so nothing is lost if the build was bad), and only then cancel.
#
# The agent runs this as three steps, because the successor has to be spawned
# by the harness's own `bash --detached` tool -- only that survives the server:
#
#   scripts/evolve.sh stage            # test, build, stage, write launcher
#   bash --detached: <the launcher it printed>
#   scripts/evolve.sh swap <stamp>     # verify successor, then cancel
#
# After `swap` this turn is cancelled and preserved; the successor binds the
# port, picks the pending turn up and asks the model to continue. That is the
# whole loop: the harness replaces the process that is running it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN="$ROOT/.dev"
HOME_DIR="${GONGGA_HOME:-$HOME/.gongga}"
BIND="${GONGGA_SERVER:-127.0.0.1:9700}"
PORT="${BIND##*:}"
CLI="$ROOT/target/debug/gongga.exe"
mkdir -p "$RUN"

die() { echo "evolve: $*" >&2; exit 1; }

# The successor must not touch the home while the old server still owns it:
# `InstanceLock` refuses, and the UDP port is still bound. Wait for both.
write_launcher() {
  local stamp="$1" exe="$2" old_pid="$3"
  local path="$RUN/evolve-$stamp-launch.sh"
  cat > "$path" <<LAUNCH
#!/usr/bin/env bash
# Successor for $stamp. Runs detached; it outlives the server it replaces.
set -u
echo \$\$ > '$RUN/evolve-$stamp-launch.pid'
NEW='$exe'
OLD_PID='$old_pid'
for _ in \$(seq 1 900); do
  alive=0
  if [ -n "\$OLD_PID" ] && tasklist //FI "PID eq \$OLD_PID" //NH 2>/dev/null | grep -q "\$OLD_PID"; then
    alive=1
  fi
  listening=\$(netstat -ano 2>/dev/null | grep -c ':$PORT' || true)
  if [ "\$alive" = 0 ] && [ "\$listening" = 0 ]; then
    cd '$ROOT'
    exec "\$NEW"
  fi
  sleep 0.2
done
echo "evolve: the old server never let go of port $PORT" >&2
exit 1
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
  # Client tests need the client, but not the client *binary*, so this does not
  # hit the lock the user's TUI puts on target/debug/gongga.exe.
  cargo test --workspace 2>&1 | tail -n 25

  echo "== build =="
  if [ "$all" = 1 ]; then
    cargo build --workspace
  else
    # Server-only by default: the TUI is running from target/debug/gongga.exe,
    # and a workspace build would fail to relink it (os error 5). The protocol
    # has not changed, so the old client still speaks to the new server.
    cargo build -p gongga-server
  fi

  local stamp exe old_pid launcher
  stamp="$(date +%Y%m%d-%H%M%S)"
  exe="$RUN/evolve-$stamp-server.exe"
  cp "$ROOT/target/debug/gongga-server.exe" "$exe" || die "copying the server failed"
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

  local launcher="$RUN/evolve-$stamp-launch.sh"
  [ -f "$launcher" ] || die "no launcher for $stamp"

  # Is something actually waiting to take over? If not, cancelling would leave
  # the home with no server at all and no successor to resume this turn. The
  # launcher records its pid before it waits, so this is a real check.
  local launch_pid
  launch_pid="$(cat "$RUN/evolve-$stamp-launch.pid" 2>/dev/null || true)"
  [ -n "$launch_pid" ] || die "successor was never launched; not cancelling"
  tasklist //FI "PID eq $launch_pid" //NH 2>/dev/null | grep -q "$launch_pid" \
    || die "successor pid $launch_pid is gone; not cancelling"

  local pid
  pid="$(running_pid || true)"
  [ -n "$pid" ] || die "no live server on $HOME_DIR; refusing to cancel"
  echo "old server pid $pid; successor $exe is waiting for the home"
  echo "cancelling: this turn is cancelled and preserved, the successor resumes it"
  "$CLI" cancel
}

cmd_status() {
  echo "server.lock: $(cat "$HOME_DIR/server.lock" 2>/dev/null || echo none)"
  netstat -ano 2>/dev/null | grep ":$PORT" || echo "port $PORT: not bound"
  ls -t "$RUN"/evolve-*.log 2>/dev/null | head -n 3
}

case "${1:-help}" in
  stage)  shift; cmd_stage "$@" ;;
  swap)   shift; cmd_swap "$@" ;;
  status) cmd_status ;;
  *)      sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
