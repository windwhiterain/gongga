#!/usr/bin/env bash
# SPEC's disk monitor, end to end and offline: a volume below the threshold the
# home declared, a tool call that succeeds anyway, and the monitor that is handed
# the report. Its own script, not a section of `monitor_selftest.sh`: it needs no
# error to have happened first, the server it starts is the whole scenario, and the
# error branch runs four minutes of turns this one has no use for.
#
# The threshold is stated in the home's own config (`[server] disk_low_mb`), which
# is what every process in the chain reads; a test cannot fill a disk, so it names
# a threshold no disk is above. The other half -- an ordinary disk says nothing,
# and a disk that stays low says it once -- is
# `disk::tests::a_low_disk_is_reported_once_and_again_after_recovering`.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# A fresh home installs nothing without a source, and the compiled default is a
# network URL this machine may not reach: the content under test is this repo's own.
export GONGGA_SHIP_URL="${GONGGA_SHIP_URL:-$root/ship}"
bin="$root/target/debug"
work="$root/.dev/disk-selftest"
home="$work/home"
log="$work/server.log"
dump="$work/fake-dump.jsonl"
roster="$work/roster.toml"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-cli" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# Every process serving THIS home, in all the spellings its command line can take:
# the supervisor re-executes the server with an absolute `--home`, so matching the
# relative one alone finds nothing and leaves a server behind.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home*$posix*' -or \$_.CommandLine -like '*--home*$win*' -or \$_.CommandLine -like '*--home*$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
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
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }
rows() {
  python - "$home" <<'PY'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
print(c.execute("select name,status,model from sessions").fetchall())
PY
}

port=$(free_port)
fake_port=$(free_port)
server_pid=""
sup=""
fake_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid" "$sup"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/fake.pid"
  sleep 1
  # Busy files mean a process still holds the home; say so instead of hiding it.
  rm -rf "$work" || echo "note: $work is still held by a process" >&2
}
trap cleanup EXIT

rm -rf "$work"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
# The threshold this machine is told to watch: no disk is above it, so the first
# tool call is already below the line, and the reading is the whole story. It goes
# into the *shipped* config, which is the base: a home whose config.toml holds only
# what the test wants is a home with no tool sentences, and the server refuses to
# start on it.
ship_config="$HOME/.gongga/ship/config/config.toml"
cp "$ship_config" "$home/config.toml"
python - "$home/config.toml" 100000000 <<'PY'
import sys
path, value = sys.argv[1], sys.argv[2]
lines = open(path, encoding="utf-8").read().splitlines()
out, done = [], False
for line in lines:
    out.append(line)
    if line.strip() == "[server]" and not done:
        out.append(f"disk_low_mb = {value}")
        done = True
assert done, "the shipped config has no [server] table"
open(path, "w", encoding="utf-8", newline="\n").write("\n".join(out) + "\n")
PY
: > "$log"; : > "$dump"

# Two flash models where the first is refused, so the disk monitor's own failure
# and the switch to a model that works are covered in the same run.
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
if ! wait_tcp "$fake_port"; then
  echo "the fake endpoint never answered on 127.0.0.1:$fake_port" >&2
  sed -n '1,12p' "$work/fake.log" >&2
  exit 1
fi

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid); sup=$(parent_of "$server_pid")
cli=("$bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port")

echo "--- the threshold is read from the home, and the first call is below it"
grep -q "threshold=104857600000000" "$log"
ok "the server read the threshold this home declared" $?

"${cli[@]}" set_session --id spill --model fake/flash-fake --cwd "$root" >/dev/null 2>&1
# A tool call that WORKS. A disk on its way out serves ordinary commands happily,
# so waiting for one of them to fail waits for the first big write -- which on a
# server that mostly reads may be never. That is the whole reason the reading
# happens after every call.
tool_ok=$("${cli[@]}" exec --session spill --bin bash --id spill/once \
  --args '["-lc", "echo hello"]' --wait 8000 2>&1)
[[ "$tool_ok" == *hello* ]]
ok "the tool call succeeded" $?

for _ in $(seq 1 100); do grep -q "below the threshold this machine declared" "$log" && break; sleep 0.3; done
grep -q "below the threshold this machine declared" "$log"
ok "and the server still reported the disk" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
rows = c.execute("select id, payload from events where kind = 'disk_space'").fetchall()
assert len(rows) == 1, f"one episode, one row: {rows}"
payload = json.loads(rows[0][1])
assert payload["session"] == "spill", payload
assert payload["tool"] == "exec", payload
assert payload["tool_ok"] is True, payload
assert payload["threshold"] > 0 and payload["volumes"], payload
INNER
ok "the event carries the session, the tool, the threshold and the volumes" $?

# The same low disk on the next call is not news: one call is not an episode.
"${cli[@]}" exec --session spill --bin bash --id spill/twice \
  --args '["-lc", "echo again"]' --wait 8000 >/dev/null 2>&1
"${cli[@]}" exec --session spill --bin bash --id spill/thrice \
  --args '["-lc", "echo and again"]' --wait 8000 >/dev/null 2>&1
python - "$home" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
rows = c.execute("select id from events where kind = 'disk_space'").fetchall()
assert len(rows) == 1, f"a disk that stays low says so once, not per call: {rows}"
INNER
ok "two more calls, still one episode" $?

echo "--- the disk monitor's standing knowledge is the cleaning skill"
for _ in $(seq 1 100); do grep -q "handed the disk's last room to the monitor" "$log" && break; sleep 0.3; done
grep -q "handed the disk's last room to the monitor" "$log"
ok "the disk event is handed to @monitor/disk" $?
rows | grep -q "'@monitor/disk'"
ok "@monitor/disk exists as an ordinary session" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("""select m.display from messages m join sessions s on s.id = m.session_id
                   where s.name = '@monitor/disk' and m.kind = 'system'
                   order by m.seq desc limit 1""").fetchone()
assert row, "the disk monitor has no prompt row"
prompt = json.loads(row[0])
rooted = prompt.get("rooted") or []
assert "@skill(clean-disk)" in rooted, f"the skill is not an index root: {rooted}"
body = prompt.get("index") or ""
assert "DryRun" in body or "clean-disk" in body, "the skill body is not in the index"
INNER
ok "the cleaning skill reaches the prompt index" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
first = c.execute("""select m.display from messages m join sessions s on s.id = m.session_id
                     where s.name = '@monitor/disk' and m.kind = 'text'
                     order by m.seq limit 1""").fetchone()[0]
text = json.loads(first).get("text", "")
assert text.startswith("@skill(clean-disk)"), text[:80]
assert "has less disk room left" in text, text[:400]
assert "Nothing has failed yet" in text, text[:400]
assert "(in session spill, which succeeded)" in text, text[:900]
assert "the threshold this machine declared" in text
assert "volumes as they stood then" in text
INNER
ok "the report names the failure, the volumes and the space that was released" $?

echo "--- a refused first model is retried, not dropped"
for _ in $(seq 1 120); do
  grep -q "handed the disk's last room to the monitor session=@monitor/disk" "$log" && break
  sleep 0.5
done
grep -q 'turn end session=@monitor/disk outcome=Failed' "$log"
ok "the disk monitor's first model was refused" $?
for _ in $(seq 1 120); do grep -q 'turn end session=@monitor/disk outcome=Done' "$log" && break; sleep 0.5; done
grep -q 'turn end session=@monitor/disk outcome=Done' "$log"
ok "and the same report on the next model finished" $?

[[ "$verbose" == "-v" ]] && { say "-- sessions:"; rows | say; }
[[ "$verbose" == "-v" ]] && { say "-- the report:"; grep -o "has less disk room[^\"]\{0,400\}" "$log" | head -1 | say; }

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · disk monitor · %s\n' "$sha"
else
  printf 'FAIL · disk monitor · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
