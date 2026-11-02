#!/usr/bin/env bash
# SPEC's monitor, end to end and offline: a fake Responses endpoint that refuses
# one model id, and a roster of two flash models where the first is the refused
# one -- so one run covers a failed turn, the handover, the monitor's own
# failure, and the switch to the model that works.
#
# One line of verdict at the end; `-v` prints the report the monitor was given.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/monitor-home"
log="$work/monitor.log"
dump="$work/monitor-dump.jsonl"
roster="$work/roster.toml"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-cli" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
# One row per session, as the monitor and the window both read them.
rows() {
  python - "$home" <<'PY'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
print(c.execute("select name,status,model,monitor_seq,monitor_attempts from sessions").fetchall())
PY
}

# Ready means the port answers, not that the lock exists: the lock is claimed
# before the home is migrated and the last content fetched.
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }

port=$(free_port)
fake_port=$(free_port)
server_pid=""
sup=""
fake_pid=""
cleanup() {
  alive "$server_pid" && taskkill //PID "$server_pid" //T //F >/dev/null 2>&1
  alive "$sup" && taskkill //PID "$sup" //T //F >/dev/null 2>&1
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/fake.pid"
  sleep 1; rm -rf "$home"
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship.git" "$home/ship.git" 2>/dev/null
: > "$log"; : > "$dump"

# Two flash models: the first is refused by the endpoint, the second is served.
# File order is the candidate order when no provider has a quota reading.
cat > "$roster" <<TOML
[[provider]]
id = "dead"
name = "Dead flash"
key_env = "DEAD_API_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

  [[provider.model]]
  id = "flash-dead"
  name = "Refused Flash"
  tier = "flash"
  context = 100000
  modalities = ["text"]

[[provider]]
id = "fake"
name = "Served flash"
key_env = "FAKE_API_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Served Flash"
  tier = "flash"
  context = 100000
  modalities = ["text"]
TOML

# Before the fake exists: it reads its knobs once, at import.
export GONGGA_PROVIDERS_CATALOG="$roster"
export DEAD_API_KEY=sk-dead
export FAKE_API_KEY=sk-fake
export FAKE_LLM_DUMP="$dump"
export FAKE_LLM_FAIL_MODEL=flash-dead

# The fake writes its own Windows pid: `$!` is an MSYS pid, and taskkill on one
# kills whatever process that number happens to name.
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake.pid" 2>/dev/null)

"$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid); sup=$(parent_of "$server_pid")
cli=("$bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port")

# Captured, not piped into `grep -q`: grep exits on the match, the command layer
# takes SIGPIPE writing the rest, and `pipefail` reports that instead.
refusal=$("${cli[@]}" set_session --session '#mine' 2>&1)
[[ "$refusal" == *"names beginning with"* ]]
ok "a person cannot take a name in the harness's namespace" $?

"${cli[@]}" set_session --session broken --model dead/flash-dead >/dev/null 2>&1
"${cli[@]}" send_message --session broken --message "say hello" >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q 'turn end session=broken' "$log" && break; sleep 0.5; done
grep -q 'turn end session=broken outcome=Failed' "$log"
ok "a session whose model is refused ends in error" $?

for _ in $(seq 1 100); do grep -q "handed a failure to the monitor" "$log" && break; sleep 0.5; done
grep -q 'handed a failure to the monitor session=broken model="dead/flash-dead"' "$log"
ok "the failure is handed to #monitor, on the first usable flash model" $?
rows | grep -q "'#monitor'"
ok "#monitor exists as an ordinary session" $?

for _ in $(seq 1 100); do
  [[ "$(grep -c 'handed a failure to the monitor' "$log")" -ge 2 ]] && break; sleep 0.5
done
grep -q 'turn end session=#monitor outcome=Failed' "$log"
ok "the monitor's own turn failed too" $?
grep -q 'handed a failure to the monitor session=#monitor model="fake/flash-fake" attempt=2' "$log"
ok "so the next model was tried" $?
for _ in $(seq 1 100); do grep -q 'turn end session=#monitor outcome=Done' "$log" && break; sleep 0.5; done
grep -q 'turn end session=#monitor outcome=Done' "$log"
ok "and that attempt finished" $?

python - "$home" <<'PY'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
texts = [json.loads(d).get("text", "") for (d,) in c.execute(
    """select m.display from messages m join sessions s on s.id = m.session_id
       where s.name = '#monitor' order by m.seq""")]
reports = [t for t in texts if "stopped with an error" in t]
assert len(reports) >= 2, reports
# The retry re-sends the first report unchanged, behind a preface naming the model.
assert "unchanged" in reports[-1], reports[-1][:200]
assert reports[-1].rstrip().endswith(reports[0].rstrip()), "the retry lost the original report"
PY
ok "the retry carries the first report, unchanged" $?

before=$(grep -c 'handed a failure to the monitor' "$log")
sleep 6
[[ "$(grep -c 'handed a failure to the monitor' "$log")" == "$before" ]]
ok "a tick does not report the same failure again" $?

[[ "$verbose" == "-v" ]] && { say "-- sessions:"; rows | say; }
[[ "$verbose" == "-v" ]] && { say "-- the report:"; grep -o "A gongga session stopped[^\"]\{0,400\}" "$log" | head -1 | say; }

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · monitor · %s\n' "$sha"
else
  printf 'FAIL · monitor · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
