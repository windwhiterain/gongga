#!/usr/bin/env bash
# The head is written once, and a head this build cannot read stops the request
# instead of being re-stamped.
#
# Seen only by a live server: the defect this guards was `Store::prompt` answering
# "no head" to a row it could not deserialize, which the caller answered by writing
# another one -- one more copy per request for a session that stayed in use. One
# home carried 137 copies, each request paired with a head write 20 ms before it.
#
# Five turns over one session, then one turn over a head whose list field is an
# explicit `null` (which is what the 6-to-7 migrator wrote). What is counted is the
# number of head rows in the transcript, which is the only place the defect shows.
#
# One line of verdict at the end. `-v` prints each step.
set -uo pipefail

native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/head-once-home"
log="$work/head-once.log"
dump="$work/head-once-dump.jsonl"
roster="$work/head-once-roster.toml"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: mbx build -p gongga-server -p gongga-cli" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
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
wait_tcp() { for _ in $(seq 1 150); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }

# A number out of the transcript, which is where the defect shows.
sql() {
  python - "$home" "$1" <<'PY'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
print(c.execute(sys.argv[2]).fetchone()[0])
PY
}

turns() { sql "select count(*) from events where kind = 'turn_end'"; }

# How many head rows one slot holds: the only place the defect shows.
head_rows() {
  local kinds="'$2'"
  [[ "$2" == instructions ]] && kinds="'instructions','system'"
  sql "select count(*) from messages where session_id = (select id from sessions where name = 'probe') and seq = $1 and kind in ($kinds)"
}

port=$(free_port)
fake_port=$(free_port)
server_pid=""
fake_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  sleep 1
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

ship="$root/ship"
export GONGGA_SHIP_URL="$(native "$ship")"
git clone --quiet --no-hardlinks "$ship" "$home/ship"
cp "$ship/config/config.toml" "$home/config.toml"
python - "$home/config.toml" <<'PY'
import sys
path = sys.argv[1]
lines = open(path, encoding="utf-8").read().splitlines()
out, done = [], False
for line in lines:
    out.append(line)
    if line.strip() == "[server]" and not done:
        out.append("disk_low_mb = 0")
        done = True
assert done, "the shipped config has no [server] table"
open(path, "w", encoding="utf-8", newline="\n").write("\n".join(out) + "\n")
PY
: > "$log"; : > "$dump"

cat > "$roster" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "FAKE_API_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

  [[provider.model]]
  id = "fake-1"
  name = "Fake One"
  tier = "flash"
  context = 200000
  modalities = ["text"]
TOML
export FAKE_API_KEY=sk-fake

FAKE_LLM_DUMP="$dump" python "$root/scripts/fake_llm.py" "$fake_port" > "$work/head-once-fake.log" 2>&1 &
fake_pid=$!
wait_tcp "$fake_port" || { echo "FAIL · the fake endpoint never answered"; exit 1; }

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI \
  -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  GONGGA_PROVIDERS_CATALOG="$(native "$roster")" GONGGA_KEY=sk-fake \
  "$bin/gongga-server" --home "$home" --bind "127.0.0.1:$port" --log info >> "$log" 2>&1 &
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; tail -20 "$log" >&2; exit 1; }
server_pid=$(child_pid)
say "server pid ${server_pid:-?} on $port; fake endpoint $fake_pid on $fake_port"

cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port" --key sk-fake)
[[ -x "${cli[0]}" ]] || cli=("$home/bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port" --key sk-fake)

created=$("${cli[@]}" '{"name":"set_session","args":{"id":"probe","model":"fake/fake-1","cwd":"'"$(native "$root")"'"}}' 2>&1)
[[ "$created" == *'"ok":true'* ]] || { echo "FAIL · session not created: $created"; exit 1; }
say "session probe created"

# A head planted the way the 6-to-7 migrator left one: five rows in their slots,
# with the list fields of the index row copied out as an explicit `null`. A build
# that reads that row as no head writes another one on every request it serves.
plant_null_head() {
  python - "$home" <<'PY'
import json, sqlite3, sys
c = sqlite3.connect(f"{sys.argv[1]}/gongga.db")
sid = c.execute("select id from sessions where name = 'probe'").fetchone()[0]
have = c.execute("select count(*) from messages where session_id = ? and seq in (1,3,4,5)", (sid,)).fetchone()[0]
if have:
    raise SystemExit(0)
now = 1
rows = [
    (1, "instructions", {"instructions": "the standing prose"}),
    (3, "agents_md", {"agents_md": "the standing rules"}),
    (4, "index", {"index": "the expanded index", "rooted": ["@file(a.md)"],
                   "index_explicit": None, "index_cached": None}),
    (5, "info", {"tail": "the card", "parameters": "shared args", "cwd": "/here",
                  "model": "fake/fake-1", "remote": None}),
]
for seq, kind, payload in rows:
    c.execute(
        "insert into messages (session_id, seq, kind, content, display, call_id, turn, step, created_at) "
        "values (?, ?, ?, NULL, ?, NULL, NULL, NULL, ?)",
        (sid, seq, kind, json.dumps(payload), now),
    )
c.commit()
PY
}
plant_null_head

say_turn() { "${cli[@]}" '{"name":"send_message","args":{"session":"probe","message":"'"$1"'"}}' >/dev/null 2>&1; }

# One turn, then four more. The count is of head rows, which must stay at one.
say_turn "first turn, say one word"
for _ in $(seq 1 300); do [[ "$(turns)" -ge 1 ]] && break; sleep 0.2; done
first=$(head_rows 1 instructions)
say "after the first turn: $first instructions row(s) at seq 1"
[[ "$first" == 1 ]]; ok "the first turn writes exactly one head" $?

for n in 2 3 4 5; do
  say_turn "turn $n, say one word"
  for _ in $(seq 1 300); do [[ "$(turns)" -ge "$n" ]] && break; sleep 0.2; done
done
after=$(head_rows 1 instructions)
say "after five turns: $after instructions row(s) at seq 1"
[[ "$after" == 1 ]]; ok "later turns add no head row" $?

slots=$(sql "select count(*) from messages where session_id = (select id from sessions where name='probe') and seq in (3,4,5) and kind in ('agents_md','index','info')")
say "head rows across slots 3, 4 and 5: $slots"
[[ "$slots" == 3 ]]; ok "each of the other slots holds exactly one row" $?

ran=$(turns)
say "turn ends recorded: $ran"
[[ "$ran" -ge 5 ]]; ok "every turn ran against the endpoint" $?

echo
echo "PASS/FAIL · ${SECONDS}s · $(git -C "$root" rev-parse --short HEAD) · failures: $fails"
[[ "$fails" == 0 ]]