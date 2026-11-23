#!/usr/bin/env bash
# The section on the arguments every call shares, on real processes: a new head
# stamps it with the tool row and the request sends it as the first message; a head
# stamped before that move keeps it in its info row and is sent before the card.
set -uo pipefail

native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
strip_cr() { python -c 'import sys;sys.stdout.write(sys.stdin.read().replace(chr(13),""))'; }
read_pid() { python -c 'import sys;print(open(sys.argv[1]).read().strip())' "$1" 2>/dev/null; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${CACHE_SELFTEST_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/params-home"
log="$work/params.log"
dump="$work/params-requests.jsonl"

fails=0
ok() { if [[ "$2" == 0 ]]; then echo "  ok   $1"; else echo "  FAIL $1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" ]] || { echo "build first: mbx build" >&2; exit 1; }
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
  # The watchers first, then the servers, all three spellings of one directory.
  local posix forward windows
  posix=$(cygpath -u "$home")
  forward=$(cygpath -m "$home")
  windows=$(cygpath -w "$home")
  # Both spellings must survive: the watcher names the bash one, and bash would
  # rewrite an argument that looks like a path.
  MSYS2_ARG_CONV_EXCL='-Posix' MSYS_NO_PATHCONV=1 \
  powershell -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -m "$root")/scripts/serving_home.ps1" \
    -Posix "$posix" -Forward "$forward" -Windows "$windows" 2>/dev/null | tr -d \r
}

port=$(free_port)
fake_port=$(free_port)
server_pid=""; fake_pid=""
cleanup() {
  # The watcher above the server goes first, or it starts another one before `rm`.
  local pid pids
  for _ in $(seq 1 10); do
    pids="$(serving_home)"
    [[ -z "${pids// /}" ]] && break
    for pid in $pids; do
      [[ "$pid" =~ ^[0-9]+$ ]] && taskkill //PID "$pid" //F >/dev/null 2>&1
    done
    sleep 1
  done
  [[ "$fake_pid" =~ ^[0-9]+$ ]] && alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/params-fake.pid"
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"
export FAKE_LLM_DUMP="$dump"
: > "$dump"

python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/params-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/params-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/params-fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(read_pid "$work/params-fake.pid")
wait_tcp "$fake_port" || { echo "the fake endpoint never answered" >&2; exit 1; }

cat > "$work/params-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "FAKE_API_KEY"
cache_ttl_secs = 0

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
export GONGGA_PROVIDERS_CATALOG="$work/params-roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI \
  -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "the server never listened" >&2; tail -20 "$log" >&2; exit 1; }
server_pid=$(read_pid "$home/server.lock")
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")
[[ -x "${cli[0]}" ]] || cli=("$home/bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port")

created=$("${cli[@]}" '{"name":"set_session","args":{"id":"fresh","model":"fake/flash-fake","cwd":"'$(native "$root")'"}}' 2>&1)
[[ "$created" == *'"ok":true'* ]]; ok "a session is created" $?
"${cli[@]}" '{"name":"send_message","args":{"session":"fresh","message":"say one word"}}' >/dev/null 2>&1
for _ in $(seq 1 240); do grep -q 'turn end session=fresh' "$log" && break; sleep 0.3; done
grep -q 'turn end session=fresh outcome=Done' "$log"; ok "its turn finishes" $?

python - "$home" "$dump" <<'INNER'
import json, sqlite3, sys
home, dump = sys.argv[1], sys.argv[2]
c = sqlite3.connect("file:" + home + "/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='fresh'").fetchone()[0]
rows = {seq: json.loads(display) for seq, display in
        c.execute("select seq, display from messages where session_id=? and seq in (2,5)", (sid,))}
section = rows[2].get("parameters")
assert section and "description" in section, rows[2]
assert rows[5].get("parameters") in (None, ""), rows[5]
bodies = [json.loads(line) for line in open(dump, encoding="utf-8") if line.strip()]
assert bodies, "no request reached the endpoint"
first = bodies[0]
texts = [part.get("text", "") if isinstance(part, dict) else part
         for item in first.get("input") or []
         for part in ([item.get("content")] if isinstance(item.get("content"), str)
                      else item.get("content") or [])]
assert texts and texts[0] == section, [t[:40] for t in texts[:2]]
assert sum(1 for t in texts if t == section) == 1, "the section is sent more than once"
opening = next((t for t in texts if "rules and references follows" in t), None)
assert opening, "the plan's opening is missing"
assert texts.index(section) < texts.index(opening), [t[:40] for t in texts[:3]]
assert first.get("tools"), "the request lost its tool block"
INNER
ok "a new head sends the section first, once, right after the tool block" $?

"${cli[@]}" '{"name":"set_session","args":{"id":"legacy","model":"fake/flash-fake","cwd":"."}}' >/dev/null 2>&1
python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1] + "/gongga.db")
fresh = c.execute("select id from sessions where name='fresh'").fetchone()[0]
legacy = c.execute("select id from sessions where name='legacy'").fetchone()[0]
assert c.execute("select count(*) from messages where session_id=? and seq in (1,2,3,4,5)",
                 (legacy,)).fetchone()[0] == 0, "the older session already has a head"
tools = json.loads(c.execute("select display from messages where session_id=? and seq=2", (fresh,)).fetchone()[0])
tools.pop("parameters", None)
rows = [
    (1, "instructions", {"instructions": ""}),
    (2, "tools", tools),
    (3, "agents_md", {"agents_md": ""}),
    (4, "index", {"plan": None, "rooted": [], "plan_explicit": [], "plan_cached": []}),
    (5, "info", {"tail": "Session `legacy`.", "parameters": "OLDER SECTION WORDS",
                 "cwd": ".", "model": "fake/flash-fake", "remote": None}),
]
for seq, kind, payload in rows:
    c.execute("insert into messages (session_id, seq, kind, content, display, call_id, turn, step, created_at) "
              "values (?, ?, ?, NULL, ?, NULL, NULL, NULL, 1)", (legacy, seq, kind, json.dumps(payload)))
c.commit()
INNER
ok "a head stamped before the move is planted with the section in its info row" $?

: > "$dump"
"${cli[@]}" '{"name":"send_message","args":{"session":"legacy","message":"say one word"}}' >/dev/null 2>&1
for _ in $(seq 1 240); do grep -q 'turn end session=legacy' "$log" && break; sleep 0.3; done
grep -q 'turn end session=legacy outcome=Done' "$log"; ok "its turn finishes" $?

python - "$dump" <<'INNER'
import json, sys
bodies = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert bodies, "the older head's request never reached the endpoint"
texts = [part.get("text", "") if isinstance(part, dict) else part
         for item in bodies[0].get("input") or []
         for part in ([item.get("content")] if isinstance(item.get("content"), str)
                      else item.get("content") or [])]
assert texts and texts[0] != "OLDER SECTION WORDS", [t[:40] for t in texts[:2]]
assert sum(1 for t in texts if t == "OLDER SECTION WORDS") == 1, [t[:40] for t in texts]
card = next(t for t in texts if "Session `legacy`" in t)
assert texts.index("OLDER SECTION WORDS") == texts.index(card) - 1, [t[:40] for t in texts[-3:]]
INNER
ok "an older head is sent as it was: its section before the card, never first" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
echo
echo "verdict ${SECONDS}s $sha failures: $fails"
[[ "$fails" == 0 ]]
