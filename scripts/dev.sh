#!/usr/bin/env bash
# Dev launcher for gongga.
#
# Why this exists: on Windows a running .exe is locked, so `cargo build` cannot
# overwrite target/debug/*.exe while a dev instance is up. The usual reaction
# is to taskkill everything named gongga — which also kills a TUI client someone
# is typing in. Instead we run *copies* outside target/, each under a unique
# name, and remember their PIDs. Rebuilds then never need to kill anything.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN="$ROOT/.dev"
mkdir -p "$RUN"

die() { echo "dev: $*" >&2; exit 1; }

# --- helpers ---------------------------------------------------------------

# Copies a built binary out of target/ under a fresh name, so an already
# running copy is never overwritten (it would be locked).
stage() {
  local src="$1" name="$2"
  [ -f "$src" ] || die "$src missing; run: dev.sh build"
  local dst="$RUN/$name-$(date +%Y%m%d-%H%M%S).exe"
  cp "$src" "$dst" || die "copying $src to $dst failed"
  echo "$dst"
}

# Starts a detached background process and records its pid.
start_bg() {
  local name="$1" exe="$2"; shift 2
  ( cd "$ROOT" && exec "$exe" "$@" ) >"$RUN/$name.log" 2>&1 &
  local pid=$!
  echo "$pid" >"$RUN/$name.pid"
  echo "$exe" >"$RUN/$name.exe"
  echo "$name started: pid $pid, log $RUN/$name.log"
}

# Opens the TUI in its own console window; it needs a real terminal, so it
# cannot be a background child of this shell.
start_window() {
  local name="$1" exe="$2"; shift 2
  cmd //c start "gongga $name" "$exe" "$@" || die "failed to open a window for $exe"
  echo "$name launched in a new window (not tracked by dev.sh stop)"
}

# Kills only what this script started, after checking the pid still belongs to
# a gongga binary. Never touches anything else.
stop_one() {
  local name="$1"
  local pidf="$RUN/$name.pid"
  [ -f "$pidf" ] || { echo "$name: not running"; return 0; }
  local pid; pid="$(cat "$pidf")"
  if ! tasklist //FI "PID eq $pid" //NH 2>/dev/null | grep -qi gongga; then
    echo "$name: pid $pid is not a gongga process anymore; clearing"
    rm -f "$pidf"
    return 0
  fi
  taskkill //PID "$pid" //F >/dev/null 2>&1 || true
  rm -f "$pidf" "$RUN/$name.exe"
  echo "$name stopped (pid $pid)"
}

# --- commands --------------------------------------------------------------

case "${1:-help}" in
  build)
    cargo build --workspace
    ;;
  server)
    stage "$ROOT/target/debug/gongga-server.exe" server >/dev/null
    exe="$(ls -t "$RUN"/server-*.exe | head -1)"
    start_bg server "$exe" "${@:2}"
    ;;
  client)
    # The `gongga` binary is the composition root (command layer + terminal), so
    # it is built by the tui crate. `-p gongga-cli` no longer produces a binary
    # on purpose: that crate is the command layer, not a program.
    cargo build -p gongga-tui || die "build failed"
    stage "$ROOT/target/debug/gongga.exe" client >/dev/null
    exe="$(ls -t "$RUN"/client-*.exe | head -1)"
    start_window client "$exe" "${@:2}"
    ;;
  bridge)
    # Long-lived like the server, so it gets the same treatment: a copy under
    # .dev/ that target/ can never lock.
    cargo build -p gongga-bridge || die "build failed"
    stage "$ROOT/target/debug/gongga-bridge.exe" bridge >/dev/null
    exe="$(ls -t "$RUN"/bridge-*.exe | head -1)"
    start_bg bridge "$exe" "${@:2}"
    ;;
  stop)
    stop_one bridge
    stop_one server
    ;;
  status)
    for name in server bridge client; do
      if [ -f "$RUN/$name.pid" ]; then
        echo "$name: pid $(cat "$RUN/$name.pid"), exe $(cat "$RUN/$name.exe" 2>/dev/null)"
      fi
    done
    tasklist //NH 2>/dev/null | grep -i gongga || echo "(no gongga processes)"
    ;;
  clean)
    # Old copies whose processes are gone. Anything still running is skipped.
    for exe in "$RUN"/*.exe; do
      [ -e "$exe" ] || continue
      rm -f "$exe" 2>/dev/null || echo "skipping locked $exe"
    done
    ;;
  *)
    sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    echo "usage: dev.sh {build|server|bridge|client|stop|status|clean}"
    ;;
esac
