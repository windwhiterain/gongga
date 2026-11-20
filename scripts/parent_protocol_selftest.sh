#!/usr/bin/env bash
# The parent-follow judgment, on real processes: a window of this protocol is
# followed (its death stops the server), a window of another protocol is not.
# `cargo test` cannot see a process die, so this covers the flag that replaced
# --parent-build. One line of verdict at the end.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
home="$root/.dev/selftest/parent-protocol-home"
log="$root/.dev/selftest/parent-protocol.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

exe=""
[[ -x "$bin/gongga-server.exe" ]] && exe="$bin/gongga-server.exe"
[[ -x "$bin/gongga-server" ]] && exe="$bin/gongga-server"
[[ -n "$exe" ]] || { echo "build first: mbx build -p gongga-server" >&2; exit 1; }

protocol=$(sed -n 's/.*PROTOCOL_VERSION: *u32 *= *\([0-9]\+\).*/\1/p' "$root/crates/protocol/src/message.rs" | head -1)
[[ -n "$protocol" ]] || { echo "cannot read PROTOCOL_VERSION" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
kill_home() { for pid in $(serving_home); do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done; }

# A killable window: sleep in this shell, read its Windows pid from /proc, and
# remember the MSYS pid to end it. tasklist and --parent both speak Windows pids.
start_window() {
  sleep 600 & win_msys=$!
  win_pid=$(cat "/proc/$win_msys/winpid" 2>/dev/null || echo "$win_msys")
}
end_window() { kill "$win_msys" 2>/dev/null || true; }

sup="" server_pid="" win_msys="" win_pid=""
cleanup() {
  end_window
  kill_home
  sleep 1
  rm -rf "$home" || echo "note: $home is still held" >&2
}
trap cleanup EXIT

port=$(free_port)
rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"

start_server() { # protocol argument value
  local proto="$1"
  rm -f "$home/server.lock"
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
    "$exe" --home "$home" --bind "127.0.0.1:$port" --log info \
      --parent "$win_pid" --parent-protocol "$proto" >>"$log" 2>&1 &
  for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
  for _ in $(seq 1 150); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
  server_pid=$(child_pid)
}

# Scenario 1: a window of this protocol is followed; its death stops the server.
start_window
say "scenario1 window(win=$win_pid msys=$win_msys) protocol=$protocol"
start_server "$protocol"
ok "scenario1 server came up" $([[ -n "$server_pid" ]] && echo 0 || echo 1)
grep -q "speaks another protocol; not following" "$log" && refused=1 || refused=0
ok "scenario1 matching protocol follows (no refusal)" $([[ $refused -eq 0 ]] && echo 0 || echo 1)
end_window
stopped=1
if [[ -z "$server_pid" ]]; then
  stopped=1  # no server to stop; the "came up" assertion carries this failure
else
  for _ in $(seq 1 100); do
    alive "$server_pid" || { stopped=0; break; }
    sleep 0.2
  done
fi
ok "scenario1 server stops when its window dies" $stopped

# Scenario 2: another protocol is not followed, so a foreign window's death is ignored.
kill_home; sleep 1; : > "$log"
start_window
say "scenario2 window(win=$win_pid msys=$win_msys) protocol=9999"
start_server 9999
ok "scenario2 server came up" $([[ -n "$server_pid" ]] && echo 0 || echo 1)
grep -q "speaks another protocol; not following" "$log" && refused=0 || refused=1
ok "scenario2 mismatched protocol refuses to follow" $refused
end_window
sleep 2
ok "scenario2 server stays up after a foreign window dies" $(alive "$server_pid" && echo 0 || echo 1)

printf 'PASS' ; [[ $fails -eq 0 ]] || printf ' (with %d failures)' "$fails"
printf ' · %s\n' "$(cd "$root" && git rev-parse --short HEAD)"
exit "$fails"
