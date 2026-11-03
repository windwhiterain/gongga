#!/usr/bin/env bash
# The supervisor, on real processes. `cargo test` cannot see a process restart, so
# this is the check that covers SPEC's watchdog: a child killed comes back, a
# refusal does not come back, and `cancel` stops the pair.
#
# One line of verdict at the end; `-v` prints what each scenario saw.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
home="$root/.dev/selftest/watchdog-home"
log="$root/.dev/selftest/watchdog.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { # name, condition-result
  if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi
}

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-cli" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# The pid of a server is the one holding its home's lock, never `$!`: Git Bash
# reports an MSYS pid, and every check below is a Windows one.
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
# The supervisor is the child's parent, which is the one relation that cannot lie.
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
# The kernel picks the port: two agents on one machine must not fight over one.
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}

# Ready means the port answers, not that the lock exists: the lock is claimed
# before the home is migrated and the last content fetched.
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }

# Every process serving THIS home, whatever the run managed to remember: a
# scenario that fails before its pid is recorded would otherwise leave a server
# behind, and the next run would mistake it for its own. Identity is the home in
# the command line, never the image name -- other servers share that.
serving_home() {
  # The home has more than one spelling: bash says `/c/...`, and the command line
  # Windows reports says `C:/...` or `C:\...`. Matching one of them finds nothing
  # and leaves a server behind, so every spelling is tried.
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

port=$(free_port)
server_pid=""
sup=""
cleanup() {
  for pid in $(serving_home) "$server_pid" "$sup"; do
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

"$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid); sup=$(parent_of "$server_pid")
say "supervisor=$sup child=$server_pid"
[[ -n "$server_pid" && "$sup" == "$(parent_of "$server_pid")" ]]
ok "one supervisor, and the child's parent is it" $?

logged=$(grep -o 'supervisor=[0-9]*' "$log" | head -1 | tr -dc '0-9')
[[ -n "$logged" && "$logged" == "$sup" ]]; ok "the server logs the supervisor the OS reports" $?

first=$server_pid
taskkill //PID "$first" //F >/dev/null 2>&1
back=""
for _ in $(seq 1 100); do
  now=$(child_pid)
  if [[ -n "$now" && "$now" != "$first" ]] && alive "$now"; then back=$now; break; fi
  sleep 0.2
done
say "restarted=$back"
[[ -n "$back" ]]; ok "a killed child comes back, with a new pid" $?
grep -q "watchdog: the server stopped without saying why" "$log"
ok "the restart is one line, and it says why" $?

server_pid=$back
# Judged by this home and these ports, never by counting `gongga-server`
# processes: another agent's server is the same image name, so a count would
# report its restart as this one's.
alt_port=$(free_port)
refusal="$root/.dev/selftest/refusal.log"; : > "$refusal"
"$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$alt_port" --log info >"$refusal" 2>&1 &
for _ in $(seq 1 150); do grep -q "already running on this home" "$refusal" && break; sleep 0.2; done
grep -q "already running on this home" "$refusal"; ok "a second server on the same home refuses" $?
[[ "$(grep -c 'watchdog:' "$refusal")" == 0 ]]; ok "a refusal is not restarted" $?
sleep 1
if netstat -ano 2>/dev/null | grep -q ":$alt_port "; then refused_left=1; else refused_left=0; fi
ok "and it left nothing listening" $refused_left
[[ "$(child_pid)" == "$server_pid" ]]; ok "the running server still serves the home" $?

"$bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port" cancel >/dev/null 2>&1
for _ in $(seq 1 100); do alive "$server_pid" || break; sleep 0.2; done
alive "$server_pid"; [[ $? != 0 ]]; ok "cancel stops the server" $?
for _ in $(seq 1 100); do alive "$sup" || break; sleep 0.2; done
alive "$sup"; [[ $? != 0 ]]; ok "cancel stops the supervisor too" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · watchdog · %s\n' "$sha"
else
  printf 'FAIL · watchdog · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
