#!/usr/bin/env bash
# The checkpoint commands at L3: `live_compact`, `memorize` and `recall` are server
# commands, not tools the model is offered. The agent loop names none of them; a
# model reaches them through `gongga_cli`, and the command commits the checkpoint
# itself. Unit tests prove the parts; only a real turn proves the model is NOT
# offered them, that `gongga_cli` routes to the command, that the command writes
# its own result row, and that the loop honors the turn-end signal.
#
# One server lifecycle, a fake provider, a table of scenarios. The pid comes from
# `server.lock`, never `$!`; the port is the kernel's; the home is this worktree's.
#
#   bash scripts/checkpoint_selftest.sh          # build first if the tree changed
#   bash scripts/checkpoint_selftest.sh -v       # keep the home and the log
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
verbose="${1:-}"

server_bin="$root/target/debug/gongga-server.exe"
[[ -x "$server_bin" ]] || { echo "no server binary at $server_bin -- run: mbx build" >&2; exit 2; }
sha="$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
base="$(basename "$root")"
home="$root/.dev/checkpoint-selftest-$base-$$"
work="$root/.dev/checkpoint-work-$base-$$"
log="$home/server.log"
dump="$work/requests.jsonl"

free_port() { python -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }
wait_tcp() {
  for _ in $(seq 1 50); do
    python -c 'import socket,sys
s=socket.socket();s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$1" && return 0
    sleep 0.2
  done
  return 1
}
alive() { [[ -n "$1" ]] && tasklist //FI "PID eq $1" 2>/dev/null | grep -q "$1"; }
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
serving_home() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
wait_listening() {
  local seen
  seen=$(grep -c "listening (UDP) local=127.0.0.1:$port" "$log" 2>/dev/null)
  seen=${seen:-0}
  for _ in $(seq 1 300); do
    [[ "$(grep -c "listening (UDP) local=127.0.0.1:$port" "$log" 2>/dev/null)" -gt "$seen" ]] && return 0
    sleep 0.2
  done
  return 1
}

failures=0
say() { printf '%s\n' "$*"; }
ok() { if [[ "$2" == 0 ]]; then say "PASS $1"; else say "FAIL $1"; failures=$((failures + 1)); fi; }

server_pid=""; fake_pid=""
cleanup() {
  # Identity by home, never by image name: kill every process whose command line
  # names this test's unique home (the watchdog parent included), plus the fake.
  local marker; marker="$(basename "$home")"
  powershell -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { \$_.CommandLine -like '*$marker*' -and \$_.ProcessId -ne \$PID } | ForEach-Object { taskkill /PID \$_.ProcessId /T /F 2>\$null }" >/dev/null 2>&1
  alive "$fake_pid" && taskkill //PID "$fake_pid" //T //F >/dev/null 2>&1
  sleep 2
  [[ "$verbose" == "-v" ]] && return 0
  rm -rf "$home" 2>/dev/null || { sleep 3; rm -rf "$home"; } || echo "note: $home is still held by a process" >&2
  rm -rf "$work"
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$work" "$home"
git clone --quiet "$root/ship" "$home/ship" 2>/dev/null || cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"
export GONGGA_HOME="$home"
export FAKE_LLM_DUMP="$dump"

port=$(free_port); fake_port=$(free_port)
cat > "$work/roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "CHECKPOINT_SELFTEST_KEY"

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
export GONGGA_PROVIDERS_CATALOG="$work/roster.toml"
export CHECKPOINT_SELFTEST_KEY=sk-fake

fake_up() {
  if [[ -n "$fake_pid" ]]; then
    alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
    for _ in $(seq 1 50); do
      python -c 'import socket,sys
s=socket.socket();s.settimeout(0.3)
sys.exit(1 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 0)' "$fake_port" && break
      sleep 0.2
    done
  fi
  rm -f "$work/fake.pid"
  env FAKE_LLM_TOOL="$1" FAKE_LLM_TOOL_ARGS="$2" \
    python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
    "$work/fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake.log" 2>&1 &
  for _ in $(seq 1 50); do [[ -s "$work/fake.pid" ]] && break; sleep 0.2; done
  fake_pid=$(cat "$work/fake.pid" 2>/dev/null)
  wait_tcp "$fake_port"
}

fake_up "" "{}" || { echo "the stand-in never answered on 127.0.0.1:$fake_port" >&2; exit 1; }
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
    -u GONGGA_API_KEY_ENV \
    "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
server_pid=$(child_pid)
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

turns=0
wait_turn() {
  turns=$((turns + 1))
  local key="turn end session=$1"
  for _ in $(seq 1 240); do
    [[ "$(grep -c "$key" "$log" 2>/dev/null)" -ge "$turns" ]] && return 0
    sleep 0.5
  done
  return 1
}

# A work session in cut mode opens its starting checkpoint before the first message.
"${cli[@]}" '{"name":"set_session","args":{"id":"main","model":"fake/flash-fake","mode":"work","cwd":"'"$(native "$root")"'","model_config":{"live_compact":{"enabled":true,"mode":"cut"}}}}' >/dev/null 2>&1
"${cli[@]}" '{"name":"send_message","args":{"message":"hello","session":"main"}}' >/dev/null 2>&1
wait_turn main

# 1. The model is offered gongga_cli, and NOT the checkpoint commands.
offered=$(python - "$dump" <<'INNER'
import json, sys
seen = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
names = {tool["name"] for tool in seen[0].get("tools") or []}
print(" ".join(sorted(names)))
INNER
)
say "offered: $offered"
ok "a turn is offered gongga_cli" "$([[ " $offered " == *" gongga_cli "* ]] && echo 0 || echo 1)"
ok "live_compact is not offered to the model" "$([[ " $offered " != *" live_compact "* ]] && echo 0 || echo 1)"
ok "memorize is not offered to the model" "$([[ " $offered " != *" memorize "* ]] && echo 0 || echo 1)"
ok "recall is not offered to the model" "$([[ " $offered " != *" recall "* ]] && echo 0 || echo 1)"

# 2. A completed ordinary round, so the checkpoint has a settled span to fold.
fake_up "exec" '{"bin":"bash","args":["-lc","echo round-one"]}'
"${cli[@]}" '{"name":"send_message","args":{"message":"do a round","session":"main"}}' >/dev/null 2>&1
wait_turn main

# 3. The model calls gongga_cli naming live_compact. The command commits the
#    checkpoint, writes its own result row, and ends the turn (continue=false).
fake_up "gongga_cli" '{"cmd":{"name":"live_compact","args":{"settled":"settled marker","next":"next marker","continue":false}}}'
"${cli[@]}" '{"name":"send_message","args":{"message":"checkpoint now","session":"main"}}' >/dev/null 2>&1
wait_turn main

counts=$(python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='main'").fetchone()[0]
cp = c.execute("select count(*) from live_compact_checkpoints where session_id=?", (sid,)).fetchone()[0]
nxt = c.execute("select count(*) from messages where session_id=? and kind='inject' and content like '%next marker%'", (sid,)).fetchone()[0]
call_id = None
for cid, display in c.execute("select call_id, display from messages where session_id=? and kind='tool_call'", (sid,)):
    d = json.loads(display)
    if d.get("name") == "gongga_cli":
        cmd = (json.loads(d.get("arguments") or "{}").get("cmd") or {})
        if cmd.get("name") == "live_compact":
            call_id = cid
res = 0
if call_id is not None:
    res = c.execute("select count(*) from messages where session_id=? and kind='tool_result' and call_id=?", (sid, call_id)).fetchone()[0]
print(cp, nxt, res)
INNER
)
read -r checkpoints next_rows result_rows <<< "$(printf '%s' "$counts" | tr -d '\r')"
say "checkpoints=$checkpoints next_rows=$next_rows result_rows=$result_rows"
ok "the live_compact command committed a checkpoint" "$([[ "$checkpoints" -ge 1 ]] && echo 0 || echo 1)"
ok "the checkpoint injected its next as a user turn" "$([[ "$next_rows" -ge 1 ]] && echo 0 || echo 1)"
ok "the command wrote its own tool_result row" "$([[ "$result_rows" -ge 1 ]] && echo 0 || echo 1)"

if [[ "$failures" -gt 0 ]]; then
  say "FAIL $failures scenario(s) · build $sha · log $log"
  exit 1
fi
say "PASS · build $sha"