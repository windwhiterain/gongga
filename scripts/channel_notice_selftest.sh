#!/usr/bin/env bash
# SPEC's channel notices across a `cancel`, on real processes: an end a call was waiting
# for owes the session a turn, and one nothing waited for is queued -- the next turn that
# runs reads it, and no turn starts for it. The failure this pins down is the second: a
# restart that woke sessions whose turns had already ended.
#
# One line of verdict at the end; `-v` prints every answer.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and a Windows program -- the
# server is one -- reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/channel-notice-home"
verbose=${1:-}
began=$(date +%s)

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
has() { [[ "$1" == *"$2"* ]]; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
server_bin="$bin/gongga-server"
[[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"
# A channel's relay has to be this server's build: the pair names both numbers otherwise.
relay_bin="$bin/gongga-relay"
[[ -x "$relay_bin" ]] || relay_bin="$bin/gongga-relay.exe"
[[ -x "$relay_bin" ]] || unset relay_bin

free_port() {
  python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}
# The server speaks UDP: a TCP dial would never answer, so readiness is the line it logs
# when the socket is up -- read from this run's own log, never one a stop left behind.
wait_listening() {
  for _ in $(seq 1 300); do
    grep -q "listening (UDP) local=127.0.0.1:$port" "$1" && return 0
    sleep 0.2
  done
  return 1
}
# The pid the server wrote for itself, never `$!`: Git Bash numbers a process its own way.
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }

# the pids a run leaves: every channel's own program, named in its directory
channel_pids() {
  for status in "$home"/channels/*/status; do
    [[ -f "$status" ]] || continue
    python -c 'import json,sys
try: print(json.load(open(sys.argv[1]))["pid"])
except Exception: pass' "$status"
  done
}
# Anything still serving this home, by the path it was told to serve.
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
cleanup() {
  for pid in $(channel_pids) "${server_pid:-}" "${fake_pid:-}" $(serving_home); do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  rm -rf "$home" 2>/dev/null || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$work" "$home"
port=$(free_port)
fake_port=$(free_port)
server_pid=""
fake_pid=""

# A session woken for a notice talks to a model; one that must not be woken would still
# have sent the request, which is half of what this measures.
export FAKE_LLM_DUMP="$work/channel-notice-dump.log"
: > "$FAKE_LLM_DUMP"
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/channel-notice-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/channel-notice-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/channel-notice-fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/channel-notice-fake.pid" 2>/dev/null)
for _ in $(seq 1 100); do
  python -c 'import socket,sys
s=socket.socket();s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$fake_port" && break
  sleep 0.2
done

cat > "$work/channel-notice-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "CHANNEL_NOTICE_SELFTEST_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Fake Flash"
  tier = "flash"
  context = 100000
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/channel-notice-roster.toml"
export CHANNEL_NOTICE_SELFTEST_KEY=sk-fake
export GONGGA_HOME="$home"

# run N: a server of this home, logging where this run can read only its own start.
run=0
start_server() {
  run=$((run + 1))
  log="$work/channel-notice-server-$run.log"
  : > "$log"
  # A session's shell exports these; a test roster cannot honour them.
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
      -u GONGGA_API_KEY_ENV -u GONGGA_SESSION GONGGA_SUPERVISED=1 \
      ${relay_bin:+GONGGA_RELAY_BIN="$relay_bin"} \
      "$server_bin" --home "$home" --bind "127.0.0.1:$port" >"$log" 2>&1 &
  wait_listening "$log" || { echo "the server never listened on 127.0.0.1:$port (run $run)" >&2; exit 1; }
  server_pid=$(child_pid)
  [[ -n "$server_pid" ]] || { echo "run $run wrote no server.lock" >&2; exit 1; }
}
start_server
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# One field of the answer, written with no trailing newline: Python on Windows ends a
# `print` with `\r\n`, and `$( )` strips only the `\n`.
field() {
  python -c 'import json,sys
text, key = sys.argv[1], sys.argv[2]
value = ""
for line in text.splitlines():
    if "cmd_result" not in line:
        continue
    message = json.loads(line)["v"]["event"].get("message", "")
    try:
        value = json.loads(message).get(key, "")
    except Exception:
        value = ""
    break
sys.stdout.write(str(value))' "$1" "$2"
}
# One field of the command's structured answer, `data`, with no trailing newline.
data_field() {
  python -c 'import json,sys
text, key = sys.argv[1], sys.argv[2]
value = ""
for line in text.splitlines():
    if "cmd_result" not in line:
        continue
    try:
        data = json.loads(line)["v"]["event"].get("data") or {}
    except Exception:
        break
    if key in data:
        value = data[key]
    break
sys.stdout.write(str(value))' "$1" "$2"
}
read_session() { "${cli[@]}" '{"name":"get_messages","args":{"session":"'"$1"'"}}' 2>&1; }
# Where a session's transcript ends, and whether a turn is running.
seq_of() { data_field "$(read_session "$1")" last_seq; }
status_of() { data_field "$(read_session "$1")" status; }
# A turn of its own, ended: what "the session finished" looks like to the store.
settled() {
  local session=$1 deadline=$((SECONDS + 60)) last="" now=""
  while (( SECONDS < deadline )); do
    if [[ "$(status_of "$session")" != running ]]; then
      now=$(seq_of "$session")
      [[ -n "$now" && "$now" == "$last" ]] && return 0
      last="$now"
    fi
    sleep 0.5
  done
  return 1
}

quiet=quiet
waited=waited
for session in $quiet $waited; do
  "${cli[@]}" '{"name":"set_session","args":{"id":"'"$session"'","model":"fake/flash-fake","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
done

# 1. A session that has finished a turn: an assistant row is on its transcript, and
#    nothing is running.
"${cli[@]}" '{"name":"send_message","args":{"session":"'"$quiet"'","message":"first turn"}}' >/dev/null 2>&1
settled "$quiet" || { echo "$quiet never finished its turn" >&2; exit 1; }
quiet_idle=$(seq_of "$quiet")
ok "a session finishes a turn before a stop" \
  "$([[ -n "$quiet_idle" && "$quiet_idle" != 0 ]] && echo 0 || echo 1)"

# 2. A channel nobody waits on: `wait: 0` is no wait at all, so the call leaves no flag.
"${cli[@]}" '{"name":"exec","args":{"session":"'"$quiet"'","bin":"bash","id":"quiet-shell","wait":0}}' >/dev/null 2>&1
listed=$("${cli[@]}" '{"name":"ls_channel","args":{"session":"'"$quiet"'","path":""}}' 2>&1)
ok "the ended session has a channel it is not waiting on" \
  "$(has "$listed" quiet-shell && echo 0 || echo 1)"

# 3. A channel somebody waits on: the call times out with no marker, which is SPEC's one
#    reason to leave the flag set.
"${cli[@]}" '{"name":"exec","args":{"session":"'"$waited"'","bin":"bash","id":"waited-shell","args":["-lc","echo up; sleep 120"],"wait":1500}}' >/dev/null 2>&1
listed=$("${cli[@]}" '{"name":"ls_channel","args":{"session":"'"$waited"'","path":""}}' 2>&1)
ok "a call that ran out of waiting leaves its channel flagged" \
  "$(has "$listed" waited-shell && echo 0 || echo 1)"
waited_before=$(seq_of "$waited")

# 4. SPEC's `cancel`: every channel ends with the server unless it is asked to keep them.
"${cli[@]}" '{"name":"cancel","args":{"keep_alive":false}}' >/dev/null 2>&1
for _ in $(seq 1 100); do alive "$server_pid" || break; sleep 0.2; done
ok "the cancel ended the server" "$(alive "$server_pid" && echo 1 || echo 0)"

# 5. The restart. `waited` is owed a turn for the end it was waiting on; `quiet` is not,
#    and a queued row must not start one for it.
start_server
# Two ticks of the wake, which is what turns an inbox row into a turn.
sleep 6
ok "the ended session is not woken by a queued notice" \
  "$([[ "$(seq_of "$quiet")" == "$quiet_idle" ]] && echo 0 || echo 1)"
ok "the session waiting on a channel is told, and continues" \
  "$([[ "$(seq_of "$waited")" -gt "$waited_before" ]] && echo 0 || echo 1)"
settled "$waited" || true
said=$(read_session "$waited")
say "$said"
ok "the notice carries what the killed channel had printed" \
  "$(has "$said" 'the server stopped this channel as it was cancelled' && echo 0 || echo 1)"

# 6. Queued is not dropped: the notice arrives with the next turn that runs, so a
#    session that never woke still reads what ended while it slept.
"${cli[@]}" '{"name":"send_message","args":{"session":"'"$quiet"'","message":"what did I miss"}}' >/dev/null 2>&1
settled "$quiet" || true
said=$(read_session "$quiet")
say "$said"
ok "the queued notice is read by the next turn" \
  "$(has "$said" 'the server stopped this channel as it was cancelled' && echo 0 || echo 1)"
ok "that turn is the message's, not the notice's" \
  "$(has "$said" 'what did I miss' && echo 0 || echo 1)"

took=$(( $(date +%s) - began ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s\n' "$took" "$sha"
else
  printf 'FAIL · %ss · %s · %s check(s)\n' "$took" "$sha" "$fails"
  exit 1
fi
