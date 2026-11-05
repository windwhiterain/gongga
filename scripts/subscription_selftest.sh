#!/usr/bin/env bash
# A subscription, on a real server: a match runs its command, the command tells the
# session that armed it, and both survive a stop.
#
# `cargo test` cannot see any of this -- it is a process, a shell and a round trip.
# What this covers, in order:
#   1. the action reaches the caller: `send_message` to `$GONGGA_SESSION` lands.
#   2. the event rides the command: kind, session and payload are readable there.
#   3. a match during the drain still fires, and an action holds the drain open --
#      the two things that make a callback as durable as the notice it replaced.
#
# One line of verdict at the end; `-v` prints what each scenario saw.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
home="$root/.dev/selftest/subscription-home"
log="$root/.dev/selftest/subscription.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { # name, condition-result
  if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi
}

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-cli" >&2; exit 1; }
[[ -x "$bin/gongga-cli.exe" || -x "$bin/gongga-cli" ]] || {
  echo "build first: cargo build -p gongga-cli" >&2; exit 1; }
server_bin="$bin/gongga-server.exe"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server"
cli_bin="$bin/gongga-cli.exe"; [[ -x "$cli_bin" ]] || cli_bin="$bin/gongga-cli"

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# The pid of a server is the one holding its home's lock, never `$!`: Git Bash
# reports an MSYS pid, and every check below is a Windows one.
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
# The kernel picks the port: two agents on one machine must not fight over one.
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}

# Every process serving THIS home, whatever this run managed to remember: identity
# is the home in the command line, never the image name -- other servers on this
# machine are the same image.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

port=$(free_port)
server_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  # Busy files mean a process still holds the home; say so instead of hiding it.
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
rm -rf "$home"; mkdir -p "$home"
# The pinned content from this machine's real cache: this check must not fetch.
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"
seen="$home/seen.txt"; : > "$seen"

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || {
  echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)
say "server pid $server_pid on $port"

cli() { "$cli_bin" --home "$home" --server "127.0.0.1:$port" "$@"; }
# A session's own shell is where `$GONGGA_SESSION` is set; here it is this script's,
# which is the same fact the server states to a shell it starts.
as() { local session="$1"; shift; GONGGA_SESSION="$session" "$cli_bin" --home "$home" --server "127.0.0.1:$port" "$@"; }

made=$(cli set_session --name parent --model deepseek-official/deepseek-flash 2>&1)
say "set_session: $made"
case "$made" in *"session is now"*) created=0 ;; *) created=1 ;; esac
ok "a session to hear the news" $created

# 1. The action reaches the caller. The command is not written here: it is the one
#    the `subagent` skill installs, read out of its own source, so a quoting mistake
#    there fails this check instead of a real parent's orchestration.
# The skill's own directory, in a spelling this machine's Python can import from:
# it is a Windows Python, and a `/c/...` path is not one it can open.
skill_dir=$(cygpath -m "$root/ship/skills/subagent" 2>/dev/null || echo "$root/ship/skills/subagent")
# The skill is a package now (`subagent/__init__.py`), so it is imported by the
# directory's own name, from the directory above it.
relay=$(python -c "import os, sys; sys.path.insert(0, os.path.dirname(sys.argv[1]));
import subagent; print(subagent._relay())" "$skill_dir")
say "relay: $relay"
as parent subscribe news '^boom$' --persistant true --bash notice --cmd "$relay" >/dev/null 2>&1
cli trigger_event boom >/dev/null 2>&1
arrived=1
heard=""
for _ in $(seq 1 100); do
  # Read once into a variable: `gongga-cli` leaves non-zero for a session that is
  # still running, and `pipefail` would report that as "no news".
  heard=$(cli get_messages --session parent -3: 2>/dev/null)
  case "$heard" in *boom*) arrived=0; break ;; esac
  sleep 0.2
done
say "heard: $heard"
ok "a match runs its command, and the command reaches the caller" $arrived
case "$heard" in *payload*) carried=0 ;; *) carried=1 ;; esac
ok "and what it carries is the event, not just its name" $carried

# 2. The event rides the command: a subscriber writes one action, before it knows
#    what will fire.
as parent subscribe probe '^probe$' --persistant true --bash notice --cmd \
  "printf '%s|%s|%s\n' \"\$GONGGA_EVENT_KIND\" \"\$GONGGA_EVENT_SESSION\" \"\$GONGGA_EVENT_PAYLOAD\" >> '$seen'" \
  >/dev/null 2>&1
cli trigger_event probe >/dev/null 2>&1
carried=1
for _ in $(seq 1 100); do
  grep -q '^probe|' "$seen" 2>/dev/null && grep -q '"kind":"probe"' "$seen" 2>/dev/null && { carried=0; break; }
  sleep 0.2
done
say "seen: $(cat "$seen" 2>/dev/null | tr '\n' ' ')"
ok "the match states its kind, its session and its payload" $carried

# 3. A match during the drain still fires, and an action holds the drain open.
#    Two actions: the first sleeps, so the process is still up when the ask lands.
: > "$seen"
as parent subscribe slow '^(slow|late)$' --persistant true --bash notice --cmd \
  "printf '%s\n' \"\$GONGGA_EVENT_KIND\" >> '$seen'; sleep 2; printf 'after-%s\n' \"\$GONGGA_EVENT_KIND\" >> '$seen'" \
  >/dev/null 2>&1
cli trigger_event slow >/dev/null 2>&1
for _ in $(seq 1 50); do grep -q '^slow$' "$seen" 2>/dev/null && break; sleep 0.1; done
cli cancel >/dev/null 2>&1
sleep 0.5
cli trigger_event late >/dev/null 2>&1
survived=1
for _ in $(seq 1 150); do
  grep -q 'after-late' "$seen" 2>/dev/null && { survived=0; break; }
  alive "$server_pid" || break
  sleep 0.2
done
say "seen after the ask: $(cat "$seen" 2>/dev/null | tr '\n' ' ')"
ok "a match during the drain still runs its action" $survived
grep -q "after-slow" "$seen" 2>/dev/null
ok "the drain waits for an action that was already running" $?

for _ in $(seq 1 150); do alive "$server_pid" || break; sleep 0.2; done
alive "$server_pid"; [[ $? != 0 ]]
ok "and the server stops once the actions are done" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · subscription · %s\n' "$sha"
else
  printf 'FAIL · subscription · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
