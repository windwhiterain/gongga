#!/usr/bin/env bash
# A server finishing a `cancel` keeps answering. A window attached to it must be able to
# ask what is happening; a silent link is indistinguishable from a dead one, and that is
# what made a cancel look like a disconnection.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/drain-home"
log="$work/drain.log"
dump="$work/drain-dump.jsonl"

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
  rm -f "$work/drain.pid"
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

mkdir -p "$work"; rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"

# A tool call that takes long enough for the drain to be observable.
export FAKE_LLM_DUMP="$dump"
export FAKE_LLM_TOOL=bash
export FAKE_LLM_TOOL_ARGS='{"command":"sleep 6","name":"default","timeout_ms":20000}'
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/drain.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/drain-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/drain.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/drain.pid" 2>/dev/null)
wait_tcp "$fake_port" || { echo "FAIL · the fake endpoint never answered"; exit 1; }

cat > "$work/drain-roster.toml" <<TOML
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
export GONGGA_PROVIDERS_CATALOG="$work/drain-roster.toml"
export FAKE_API_KEY=sk-fake

"$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; exit 1; }

cli=("$bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port")
created=$("${cli[@]}" set_session --session sleeper --cwd "$work" --model fake/flash-fake 2>&1)
[[ "$created" == *'"ok":true'* ]] || { echo "FAIL · session not created: $created" >&2; exit 1; }

# A turn that is still running when `cancel` lands: that is what makes a drain.
"${cli[@]}" send_message --session sleeper --message "run the slow command" >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q "tool start session=sleeper" "$log" && break; sleep 0.2; done
grep -q "tool start session=sleeper" "$log" || { echo "FAIL · the turn never started"; exit 1; }

cancelled=$("${cli[@]}" cancel 2>&1)
[[ "$cancelled" == *'"ok":true'* ]] || { echo "FAIL · cancel refused: $cancelled" >&2; exit 1; }
ok "cancel is acknowledged" $?

# The point of the whole change: while the turn is still finishing, the server answers.
answered=""
for _ in $(seq 1 25); do
  # Still draining: the process is up and no "server stopped" line yet.
  grep -q "server stopped" "$log" && break
  answered=$("${cli[@]}" get_messages --session sleeper --range -1:-1 2>&1)
  [[ "$answered" == *'"ok":true'* ]] && break
  sleep 0.2
done
[[ "$answered" == *'"ok":true'* ]]
ok "a draining server still answers a command" $?

# And it says so, rather than leaving a window to guess.
grep -q "the server is stopping" "$log"
ok "the drain is announced on the event stream" $?

# The turn really did finish before the process left: waited for, not raced. The
# drain is what makes the wait long, and this is the assertion that says it ended.
for _ in $(seq 1 100); do grep -q "turn end session=sleeper outcome=Cancelled" "$log" && break; sleep 0.2; done
grep -q "turn end session=sleeper outcome=Cancelled" "$log"
ok "the turn finished, then the server stopped" $?

for _ in $(seq 1 100); do grep -q "server stopped" "$log" && break; sleep 0.3; done
grep -q "server stopped" "$log"
ok "the server exits once its turns are done" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · drain · %s\n' "$sha"
else printf 'FAIL · drain · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1; fi
