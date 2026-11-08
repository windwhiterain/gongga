#!/usr/bin/env bash
# SPEC: "auto recover invalid transcript (only append, no drop)".
#
# A run killed inside a tool leaves a `function_call` no item answers -- a transcript a
# provider rejects, the same way on every later turn. The next start answers it by
# *appending* a row, so recovery stays append-only: the answer is stored once and is the
# same answer for every request, rather than something a request invents each time.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/validity-home"
log="$work/validity.log"
dump="$work/validity-dump.jsonl"

fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
dial_ready() {
  python -c 'import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$1"
}
wait_tcp() { for _ in $(seq 1 100); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

port=$(free_port); fake_port=$(free_port); fake_pid=""
cleanup() {
  for pid in $(serving_home); do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/validity.pid"
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

mkdir -p "$work"; rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"

# A tool that outlives the kill, so the call really is left open.
export FAKE_LLM_DUMP="$dump"
export FAKE_LLM_TOOL=bash
export FAKE_LLM_TOOL_ARGS='{"command":"sleep 60","name":"default","timeout_ms":120000}'
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/validity.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/validity-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/validity.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/validity.pid" 2>/dev/null)
wait_tcp "$fake_port" || { echo "FAIL · the fake endpoint never answered"; exit 1; }

cat > "$work/validity-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "FAKE_API_KEY"

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
export GONGGA_PROVIDERS_CATALOG="$work/validity-roster.toml"
export FAKE_API_KEY=sk-fake

start() { # logfile-to-append-to
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$1" 2>&1 &
  for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$1" && break; sleep 0.2; done
}

start "$log"
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; exit 1; }
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")
"${cli[@]}" set_session --name v --model fake/flash-fake --cwd "$work" >/dev/null 2>&1
"${cli[@]}" send_message --name v --message "run it" >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q "tool start session=v" "$log" && break; sleep 0.2; done
grep -q "tool start session=v" "$log" || { echo "FAIL · the turn never started a tool"; exit 1; }

# The crash: no cancel, no warning, no chance to write an answer.
pid=$(cat "$home/server.lock")
taskkill //PID "$pid" //T //F >/dev/null 2>&1
sleep 2

left=$(python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
calls = {json.loads(d)["call_id"] for (d,) in c.execute(
    "select display from messages where kind='tool_call'")}
answered = {json.loads(d)["call_id"] for (d,) in c.execute(
    "select display from messages where kind='tool_result'")}
print(len(calls - answered))
INNER
)
[[ "${left:-0}" -ge 1 ]]
ok "a run killed inside a tool leaves a call unanswered" $?

# The next start repairs before anything serves.
: > "$log"
start "$log"
grep -q "transcript repaired before serving" "$log"
ok "the next start repairs it before serving" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
calls = {json.loads(d)["call_id"] for (d,) in c.execute(
    "select display from messages where kind='tool_call'")}
answered = {json.loads(d)["call_id"] for (d,) in c.execute(
    "select display from messages where kind='tool_result'")}
missing = calls - answered
assert not missing, f"a request would arrive with no answer for {sorted(missing)}"
# And it was appended: the answer is a row of its own, after the call it answers.
rows = list(c.execute("select seq, kind from messages order by seq"))
call_at = next(seq for seq, kind in rows if kind == "tool_call")
assert any(kind == "tool_result" and seq > call_at for seq, kind in rows), rows
INNER
ok "the call is answered by a row appended after it" $?

# A second start finds nothing left to do: the repair is not repeated.
: > "$log"
pid=$(cat "$home/server.lock"); alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
sleep 2
start "$log"
grep -q "transcript repaired before serving" "$log" && repaired=1 || repaired=0
[[ "$repaired" == 0 ]]
ok "a later start has nothing to repair" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · validity · %s\n' "$sha"
else printf 'FAIL · validity · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1; fi
