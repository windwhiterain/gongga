#!/usr/bin/env bash
# A provider's `models_script` prints its model list as JSON, so a relay with too
# many models to name by hand (OpenCode Go) is discovered instead of copied. This
# runs that contract on real processes, against the fake Responses endpoint.
#
# Scenarios, one server lifecycle:
#   dyn   no static rows, only a script: its two models become selectable
#   merge static rows plus a script: ids join, a shared id keeps the static row
#   fail  a script that exits nonzero: the static row still works, the server lives
#   e2e   a scripted model sends a real completion through to the fake endpoint
#
# One line of verdict at the end.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and a Windows program -- the
# server is one -- reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${CACHE_SELFTEST_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/models-script-home"
log="$work/models-script.log"

fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
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
wait_listening() { for _ in $(seq 1 300); do grep -q "listening.*127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }

port=$(free_port)
fake_port=$(free_port)
server_pid=""; fake_pid=""
cleanup() {
  alive "$server_pid" && taskkill //PID "$server_pid" //T //F >/dev/null 2>&1
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/fake-ms.pid"
  # The killed server's handles take a moment to close; retry instead of crying held.
  for _ in $(seq 1 10); do rm -rf "$home" 2>/dev/null; [[ -d "$home" ]] || break; sleep 0.5; done
  [[ -d "$home" ]] && echo "note: $home is still held" >&2
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$home" "$work/providers"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"

export FAKE_LLM_DUMP="$work/ms-requests.jsonl"
: > "$FAKE_LLM_DUMP"

python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/fake-ms.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake-ms.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/fake-ms.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake-ms.pid" 2>/dev/null)
if ! wait_tcp "$fake_port"; then
  echo "the fake endpoint never came up" >&2; sed -n '1,12p' "$work/fake-ms.log" >&2; exit 1
fi

# Discovery scripts: good ones print a FileModel array, the bad one exits nonzero.
cat > "$work/providers/good.py" <<'PY'
import json
print(json.dumps([
  {"id": "dyn-a", "name": "Dynamic A", "context": 100000, "modalities": ["text"]},
  {"id": "dyn-b", "name": "Dynamic B", "context": 50000, "modalities": ["text"]},
]))
PY
cat > "$work/providers/good2.py" <<'PY'
import json
print(json.dumps([
  {"id": "shared", "name": "From Script"},
  {"id": "scripted", "name": "Only Script"},
]))
PY
cat > "$work/providers/bad.py" <<'PY'
import sys
sys.stderr.write("the discovery blew up")
sys.exit(1)
PY

cat > "$work/roster.toml" <<TOML
[[provider]]
id = "dyn"
name = "Purely Dynamic"
key_env = "FAKE_API_KEY"
models_script = "providers/good.py"
  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

[[provider]]
id = "merge"
name = "Static And Script"
key_env = "FAKE_API_KEY"
models_script = "providers/good2.py"
  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"
  [[provider.model]]
  id = "static-only"
  name = "Only Static"
  modalities = ["text"]
  [[provider.model]]
  id = "shared"
  name = "Static Wins"
  modalities = ["text"]

[[provider]]
id = "fail"
name = "Broken Script"
key_env = "FAKE_API_KEY"
models_script = "providers/bad.py"
  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"
  [[provider.model]]
  id = "keep"
  name = "Kept Static"
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened" >&2; exit 1; }
server_pid=$(child_pid)
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# dyn: a script with no static rows makes both printed models selectable.
"${cli[@]}" '{"name":"set_session","args":{"id":"s1","model":"dyn/dyn-a","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
ok "a scripted model is selectable with no static rows" $?
"${cli[@]}" '{"name":"set_session","args":{"id":"s1b","model":"dyn/dyn-b","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
ok "the second scripted model is selectable" $?
missing=$("${cli[@]}" '{"name":"set_session","args":{"id":"sx","model":"dyn/nope","cwd":"'"$(native "$root")"'"}}' 2>&1)
[[ "$missing" == *"dyn-a"* && "$missing" == *"dyn-b"* ]]
ok "an unknown id is refused and the known scripted ids are listed" $?

# merge: static rows and scripted rows join; the shared id keeps the static row.
"${cli[@]}" '{"name":"set_session","args":{"id":"s2","model":"merge/static-only","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
ok "a static-only model still resolves beside a script" $?
"${cli[@]}" '{"name":"set_session","args":{"id":"s3","model":"merge/scripted","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
ok "a script-only model resolves alongside static rows" $?
"${cli[@]}" '{"name":"set_session","args":{"id":"s4","model":"merge/shared","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
ok "the shared id resolves (static row kept, not duplicated)" $?

# fail: a nonzero exit leaves the static row usable and does not kill the server.
"${cli[@]}" '{"name":"set_session","args":{"id":"s5","model":"fail/keep","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
ok "a failed script leaves the static model usable" $?
grep -q "model script failed" "$log"
ok "the failed script is logged as a warning" $?

# e2e: a completion on a scripted model reaches the fake endpoint.
"${cli[@]}" '{"name":"set_session","args":{"id":"e2e","model":"dyn/dyn-a","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
"${cli[@]}" '{"name":"send_message","args":{"session":"e2e","message":"hello dynamic"}}' >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q 'turn end session=e2e' "$log" && break; sleep 0.3; done
grep -q '"model":"dyn-a"' "$FAKE_LLM_DUMP"
ok "a completion on a scripted model sends to the endpoint" $?

if [[ "$fails" == 0 ]]; then
  echo "PASS - models_script selftest"
  exit 0
fi
echo "FAIL - $fails check(s) failed"
exit 1
