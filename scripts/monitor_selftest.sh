#!/usr/bin/env bash
# SPEC's monitor, end to end and offline: a fake Responses endpoint that refuses
# one model id, and a roster of two flash models where the first is the refused
# one -- so one run covers a failed turn, the handover, the monitor's own
# failure, and the switch to the model that works. The disk kind has its own
# script (`disk_selftest.sh`): it needs no error to have happened first.
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
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# Every process serving THIS home, whatever the run remembers: a scenario that
# fails before its pid is recorded would otherwise leave a server behind. The
# home has several spellings -- bash's `/c/...`, Windows' `C:/...` -- and matching
# one of them finds nothing, so all of them are tried.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
# A TCP port nobody holds. UDP would answer a different question, and a port the
# kernel hands back for UDP can already belong to a service -- measured: one run
# picked a port `svchost` was listening on, and the fake endpoint died on bind.
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
# Ready means it answers, not that a process exists.
dial_ready() {
  python -c 'import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$1"
}
wait_tcp() { for _ in $(seq 1 100); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }
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
  for pid in $(serving_home) "$server_pid" "$sup"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/fake.pid"
  sleep 1
  # Busy files mean a process still holds the home; say so instead of hiding it.
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
# The installed content is the clone this home can resolve commits from; the prompt
# words this checkout carries are laid over it below, because the binary under test
# is built from this tree and the report's wording is what this run reads.
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
# This home watches no disk: the disk kind's own script states its threshold. The
# shipped config is the base, because a home config that holds only this is a home
# with no tool sentences, and the server refuses to start on it.
cp "$HOME/.gongga/ship/config/config.toml" "$home/config.toml"
python - "$root/ship/config/config.toml" "$home/config.toml" <<'OVERLAY'
import re
import sys

carried = open(sys.argv[1], encoding="utf-8").read()
target = sys.argv[2]
installed = open(target, encoding="utf-8").read()
for name in ("monitor_error", "monitor_error_again"):
    found = re.search(rf"^{name} = '''.*?'''", carried, re.S | re.M)
    if found is None:
        sys.exit(f"the checkout's config carries no {name}")
    installed = re.sub(rf"^{name} = '''.*?'''", found.group(0), installed, count=1, flags=re.S | re.M)
open(target, "w", encoding="utf-8", newline="\n").write(installed)
OVERLAY
# The skill this checkout carries, installed where a home resolves it: publishing
# does the same thing through the content repository, which this run does not reach.
mkdir -p "$home/skills"
rm -rf "$home/skills/providers"
cp -r "$root/ship/skills/providers" "$home/skills/providers"
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
# One MiB: the test checks the claim, not the arithmetic of a gigabyte. A volume
# with less than this left is the "full" the free space reading has to see, and a
# tool that says so itself is the other reading.


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
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# Captured, not piped into `grep -q`: grep exits on the match, the command layer
# takes SIGPIPE writing the rest, and `pipefail` reports that instead.
refusal=$("${cli[@]}" set_session '@mine' --model dead/flash-dead --cwd "$root" 2>&1)
[[ "$refusal" == *"names beginning with"* ]]
ok "a person cannot take a name in the harness's namespace" $?

"${cli[@]}" set_session broken --model dead/flash-dead --cwd "$root" >/dev/null 2>&1
"${cli[@]}" send_message --name broken "say hello" >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q 'turn end session=broken' "$log" && break; sleep 0.5; done
grep -q 'turn end session=broken outcome=Failed' "$log"
ok "a session whose model is refused ends in error" $?

for _ in $(seq 1 100); do grep -q "handed a failure to the monitor" "$log" && break; sleep 0.5; done
grep -q 'handed a failure to the monitor session=broken model=dead/flash-dead' "$log"
ok "the failure is handed to @monitor/providers, on the first usable flash model" $?
rows | grep -q "'@monitor/providers'"
ok "@monitor/providers exists as an ordinary session" $?

for _ in $(seq 1 100); do grep -q 'turn end session=@monitor/providers outcome=Failed' "$log" && break; sleep 0.5; done
grep -q 'turn end session=@monitor/providers outcome=Failed' "$log"
ok "the monitor's own turn failed too" $?
# Waited for by its own line, not by "a second handover happened": the monitor's
# failure and the retry are two handovers, and the second one lands after both.
retry='handed a failure to the monitor session=@monitor/providers model=fake/flash-fake attempt=2'
for _ in $(seq 1 100); do grep -q "$retry" "$log" && break; sleep 0.5; done
grep -q "$retry" "$log"
ok "so the next model was tried" $?
for _ in $(seq 1 100); do grep -q 'turn end session=@monitor/providers outcome=Done' "$log" && break; sleep 0.5; done
grep -q 'turn end session=@monitor/providers outcome=Done' "$log"
ok "and that attempt finished" $?

echo "--- the standing knowledge is an index entry, not a message"
python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("""select m.display from messages m join sessions s on s.id = m.session_id
                   where s.name = '@monitor/providers' and m.kind = 'system'
                   order by m.seq desc limit 1""").fetchone()
assert row, 'the monitor session has no prompt row'
prompt = json.loads(row[0])
rooted = prompt.get('rooted') or []
assert '@skill(providers)' in rooted, f'the skill is not an index root: {rooted}'
body = prompt.get('index') or ''
assert 'provider 4xx' in body or 'providers' in body, 'the skill body is not in the index'
assert 'The text of every reference' in body
INNER
ok "the skill body reaches the prompt index, not the transcript" $?

echo "--- the report is the reference plus this failure's facts"
python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
first = c.execute("""select m.display from messages m join sessions s on s.id = m.session_id
                     where s.name = '@monitor/providers' and m.kind = 'text'
                     order by m.seq limit 1""").fetchone()[0]
text = json.loads(first).get('text', '')
assert text.startswith('@skill(providers)'), text[:80]
assert 'A gongga session stopped because a provider would not serve it' in text
assert 'session: broken' in text, 'the report names the session that failed'
assert 'harness build:' in text and 'home:' in text
# The literal spelling stays in the message: the index carries the body instead.
assert 'provider 4xx' not in text
INNER
ok "the report names this failure and leaves the map to the index" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
texts = [json.loads(d).get("text", "") for (d,) in c.execute(
    """select m.display from messages m join sessions s on s.id = m.session_id
       where s.name = '@monitor/providers' order by m.seq""")]
reports = [t for t in texts if "stopped because a provider would not serve it" in t]
assert len(reports) >= 2, reports
# The retry re-sends the first report unchanged, behind a preface naming the
# model. It is the row that says so: a later failure of the same session earns
# its own fresh report, which is a different row.
retry = next((t for t in reports if "unchanged" in t), None)
assert retry is not None, "no retry report arrived"
assert retry.rstrip().endswith(reports[0].rstrip()), "the retry lost the original report"
INNER
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
