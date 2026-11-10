#!/usr/bin/env bash
# The tools as an *agent* meets them: a stand-in model in a real turn, using the rows
# this build offers. The command-level selftests say a tool answers when it is called;
# only a turn says whether the model is told about it, and whether the round trip an
# agent actually needs -- start a channel, read it, then stay for what a subscription
# sends -- lands.
#
# One line of verdict at the end; `-v` keeps the scratch and prints each turn.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
# A run's own scratch: two of these on one machine must not share a home, or one
# run's cleanup deletes the other's channel while it waits on it.
tag="tool-$$"
work="$root/.dev/selftest/$tag"
home="$work/home"
log="$work/tool.log"
dump="$work/tool-requests.jsonl"
verbose=${1:-}
began=$(date +%s)
marker="tool-selftest-$$"

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
has() { [[ "$1" == *"$2"* ]]; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
server_bin="$bin/gongga-server"
[[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"

free_port() {
  python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}
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
wait_tcp() {
  for _ in $(seq 1 100); do
    python -c 'import socket,sys
s=socket.socket();s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$1" && return 0
    sleep 0.2
  done
  return 1
}
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
server_pid=""
fake_pid=""
started=""
channel=""
second=""
waited=""
cleanup() {
  for pid in $(serving_home) "$server_pid" "$fake_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 2
  [[ "$verbose" == "-v" ]] && return 0
  rm -rf "$home" 2>/dev/null || { sleep 3; rm -rf "$home"; } || echo "note: $home is still held by a process" >&2
  rm -rf "$work"
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$work" "$home"
# This checkout's own content: a home with none reaches for the network.
git clone --quiet "$root/ship" "$home/ship" 2>/dev/null || cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"
export GONGGA_HOME="$home"
export FAKE_LLM_DUMP="$dump"

port=$(free_port)
fake_port=$(free_port)
cat > "$work/tool-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "TOOL_SELFTEST_KEY"

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
export GONGGA_PROVIDERS_CATALOG="$work/tool-roster.toml"
export TOOL_SELFTEST_KEY=sk-fake

# The stand-in is restarted on the same port between turns: `FAKE_LLM_TOOL` is its
# environment, and one turn is the unit that needs a different tool.
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
  rm -f "$work/tool-fake.pid"
  env FAKE_LLM_TOOL="$1" FAKE_LLM_TOOL_ARGS="$2" \
    python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
    "$work/tool-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/tool-fake.log" 2>&1 &
  for _ in $(seq 1 50); do [[ -s "$work/tool-fake.pid" ]] && break; sleep 0.2; done
  fake_pid=$(cat "$work/tool-fake.pid" 2>/dev/null)
  wait_tcp "$fake_port"
}

fake_up "" "{}" || { echo "the stand-in never answered on 127.0.0.1:$fake_port" >&2; exit 1; }
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
    -u GONGGA_API_KEY_ENV -u GONGGA_SESSION \
    "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
server_pid=$(child_pid)
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# A turn is counted, not timed: the stand-in answers in milliseconds, so a count
# taken after `send_message` returns can already include the turn it is waiting for.
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

# What one tool answered in the turn just ended: the newest tool result's output.
last_tool() {
  python - "$home" "$1" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("select id from sessions where name=?", (sys.argv[2],)).fetchone()
if row is None:
    print("")
    sys.exit(0)
for (display,) in c.execute(
        "select display from messages where session_id=? and kind='tool_result' order by seq desc limit 1",
        (row[0],)):
    print(json.loads(display).get("output", ""))
INNER
}

"${cli[@]}" set_session --id main --model fake/flash-fake --cwd "$root" >/dev/null 2>&1
"${cli[@]}" send_message --message "hello" --session main >/dev/null 2>&1
wait_turn main
fake_up "" "{}"

# 1. The table the model is offered. This is the assertion the command-level tests
#    cannot make: a tool nobody calls because nobody was told about it looks fine.
offered=$(python - "$dump" <<'INNER'
import json, sys
seen = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
names = {tool["name"] for tool in seen[0].get("tools") or []}
print(" ".join(sorted(names)))
INNER
)
say "offered: $offered"
ok "a turn is offered exec" "$([[ " $offered " == *" exec "* ]] && echo 0 || echo 1)"
ok "a turn is offered wait, so a session can stay for its subscription" \
  "$([[ " $offered " == *" wait "* ]] && echo 0 || echo 1)"
ok "the old shell tools are not offered to the model" \
  "$([[ " $offered " != *" bash "* && " $offered " != *" read_file "* ]] && echo 0 || echo 1)"

# 2. `exec`: the model runs a program and is answered with the channel it opened.
#    Persistent on purpose -- the next scenario has to talk to it -- and `wait 0`,
#    because a shell that has just been opened has said nothing yet.
fake_up exec '{"bin":"bash","wait":0}' || exit 1
"${cli[@]}" send_message --message "start a channel" --session main >/dev/null 2>&1
wait_turn main
started=$(last_tool main)
say "exec answered: $(printf '%s' "$started" | head -c 200)"
ok "exec answered the model with the channel it opened" "$(has "$started" '"id":"' && echo 0 || echo 1)"

# 3. `exec` again, by `id` alone: SPEC's `bin` defaults to `channel[id].bin`, so the
#    second call speaks to the channel the first one opened.
channel=$(python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("select id from sessions where name='main'").fetchone()
for (display,) in c.execute(
        "select display from messages where session_id=? and kind='tool_result' order by seq",
        (row[0],)):
    text = json.loads(display).get("output", "")
    start = text.find("{")
    if start < 0:
        continue
    try:
        value, _ = json.JSONDecoder().raw_decode(text[start:])
    except Exception:
        continue
    if isinstance(value, dict) and value.get("id"):
        print(value["id"])
        break
INNER
)
say "channel: $channel"
fake_up exec "$(python -c 'import json,sys
print(json.dumps({"id": sys.argv[1], "input": "echo second", "wait": 15000}))' "$channel")" || exit 1
"${cli[@]}" send_message --message "say second in it" --session main >/dev/null 2>&1
wait_turn main
second=$(last_tool main)
say "exec answered: $(printf '%s' "$second" | head -c 200)"
ok "the second exec spoke to the channel the first opened" "$(has "$second" 'second' && echo 0 || echo 1)"

# 4. The round trip the whole row exists for: arm a subscription, then `wait` -- the
#    news it sends cuts the wait, and arrives as the next turn's message.
armed=$(GONGGA_SESSION=main "${cli[@]}" subscribe --pattern "^$marker\$" \
  --cmd '{"name": "send_message", "args": {"message": "news-from-sub", "session": "main"}}' \
  --id "$marker" --persistant true 2>&1)
say "$armed"
fake_up wait '{"time":30000}' || exit 1
"${cli[@]}" send_message --message "wait for the news" --session main >/dev/null 2>&1
sleep 2
"${cli[@]}" trigger_event --event "$marker" >/dev/null 2>&1
wait_turn main
waited=$(last_tool main)
say "wait answered: $(printf '%s' "$waited" | head -c 200)"
# SPEC gives `wait` a `time` back, so "the news cut it" is a number well under the
# 30 s this call asked for: the ceiling itself would be 30.
waited_s=$(printf '%s' "$waited" | head -1 | sed -n 's/^\([0-9][0-9]*\)\.[0-9]*s$/\1/p')
ok "wait ended on the news a subscription sent"   "$([[ -n "$waited_s" && $waited_s -lt 30 ]] && echo 1 || echo 0)"
# The news itself is a message, which is what makes the wait worth anything.
"${cli[@]}" get_messages --session main 2>/dev/null | grep -q 'news-from-sub'
ok "the news reached the session as a message" "$?"

# 5. A call's own clock. `wait` is the one tool whose wait is its work, so the
#    `time` a call names is the ceiling it gets -- the row's default is what a
#    call that names none gets. Measured before this test: 5 s asked, 120 s waited.
fake_up wait '{"time":2500}' || exit 1
"${cli[@]}" send_message --message "wait a moment" --session main >/dev/null 2>&1
wait_turn main
short=$(last_tool main)
say "short wait answered: $(printf '%s' "$short" | head -c 120)"
ok "wait honours the clock its call names" \
  "$(has "$short" 'within 2.5s' && echo 0 || echo 1)"

took=$(( $(date +%s) - began ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s\n' "$took" "$sha"
else
  printf 'FAIL · %ss · %s · %s check(s)\n' "$took" "$sha" "$fails"
  exit 1
fi
