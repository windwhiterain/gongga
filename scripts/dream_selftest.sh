#!/usr/bin/env bash
# SPEC's Keep Alive, on real processes: a cache window sends one notice naming the
# dreamer and handoff skills, nothing is forked by the server, an explicit mute is
# quiet until any message clears it, `continue=false` writes context without a
# turn, and legacy dream_of rows left by older builds are still swept at startup.
#
# One home, one fake endpoint; the cancel/restart and sweep scenarios restart the
# server on the same home, which is what they are about. One line of verdict at
# the end; `-v` prints what each scenario saw.
set -uo pipefail

native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/dream-home"
log="$work/dream.log"
dump="$work/dream-dump.jsonl"
roster="$work/dream-roster.toml"
verbose=${1:-}
started=$(date +%s)

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# The pid of a server is the one holding its home's lock, never `$!`: Git Bash
# reports an MSYS pid, and every check below is a Windows one.
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
# Every process serving THIS home: identity is the home in the command line,
# never the image name -- other servers share that.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
# A TCP port nobody holds: the kernel picks, two agents must not fight over one.
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

# Read-only rows out of the home's database: the evidence is what was stored.
q() {
  python - "$home" "$1" <<'PY'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
for row in c.execute(sys.argv[2]):
    print("|".join("" if v is None else str(v) for v in row))
PY
}
# Poll a query until its answer contains the wanted text.
poll() { local i; for i in $(seq 1 "${3:-150}"); do [[ "$(q "$1")" == *"$2"* ]] && return 0; sleep 0.2; done; return 1; }
# Poll a query until it answers nothing.
poll_gone() { local i; for i in $(seq 1 "${2:-150}"); do [[ -z "$(q "$1")" ]] && return 0; sleep 0.2; done; return 1; }
# Poll the fake's dump until one request carries both texts: another session's
# heartbeat may interleave, so the last line is not the one to read. Two steps,
# never a pipe into `grep -q`: pipefail reads its early exit as a failure.
poll_dump() {
  local i
  for i in $(seq 1 "${3:-100}"); do
    grep "$1" "$dump" > "$work/dream-match.jsonl" 2>/dev/null
    grep -q "$2" "$work/dream-match.jsonl" && return 0
    sleep 0.2
  done
  return 1
}

port=$(free_port)
fake_port=$(free_port)
key="dream-selftest-key"
server_pid=""
sup=""
fake_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid" "$sup"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/dream-fake.pid"
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
mkdir -p "$work"
rm -rf "$home"; mkdir -p "$home"
# The checkout under test, not the running home: a worktree has its own submodule.
export GONGGA_SHIP_URL="$(native "$root/ship")"
git clone --quiet --no-hardlinks "$root/ship" "$home/ship"
# The home reads the content's own config file, so the test knobs ride the
# roster (a declared short tier) and the environment (no disk watch), never a
# copied home config: that one is kept and read by nobody.
export GONGGA_DISK_LOW_MB=0
: > "$log"; : > "$dump"

# Two models on one fake endpoint: the default the startup probe asks for, and
# the one the sessions name. File order is the candidate order.
cat > "$roster" <<TOML
[[provider]]
id = "deepseek-official"
name = "Default"
key_env = "DREAM_DEFAULT_KEY"
cache_ttl_tiers = { "2s" = 2 }

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

  [[provider.model]]
  id = "deepseek-flash"
  name = "Default Flash"
  tier = "flash"
  context = 100000
  modalities = ["text"]

[[provider]]
id = "fake"
name = "Served flash"
key_env = "FAKE_API_KEY"
cache_ttl_tiers = { "2s" = 2 }

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
export DREAM_DEFAULT_KEY=sk-default
export FAKE_API_KEY=sk-fake
export FAKE_LLM_DUMP="$dump"
# A slow answer in pieces: a cancel can land mid-stream, which is scenario (b).
export FAKE_LLM_SLOW_PIECES="alpha|beta|gamma"
export FAKE_LLM_SLOW_PAUSE="0.8"

# The fake writes its own Windows pid: `$!` is an MSYS pid.
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/dream-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/dream-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/dream-fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/dream-fake.pid" 2>/dev/null)
wait_tcp "$fake_port" || { echo "the fake endpoint never answered on 127.0.0.1:$fake_port" >&2; exit 1; }

cli() { bash "$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port" --key "$key" "$1"; }

# Ready means the greeting answers, not that the lock exists: the lock is claimed
# before the home is migrated and the content fetched.
start_server() {
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
    -u GONGGA_API_KEY_ENV GONGGA_KEY="$key" \
    "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
  local i
  for i in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
  for i in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
  grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "the server never listened on 127.0.0.1:$port" >&2; return 1; }
  server_pid=$(child_pid); sup=$(parent_of "$server_pid")
  for i in $(seq 1 100); do cli '{"name":"ls_session","args":{}}' >/dev/null 2>&1 && return 0; sleep 0.2; done
  echo "the server never answered a dial" >&2; return 1
}
stop_server() {
  cli '{"name":"cancel","args":{}}' >/dev/null 2>&1
  local i
  for i in $(seq 1 100); do alive "$server_pid" || break; sleep 0.2; done
  alive "$server_pid" && { echo "the server did not stop" >&2; return 1; }
  server_pid=""; sup=""
}

start_server || exit 1
say "server=$server_pid fake=$fake_pid port=$port"

# --- (a) a fire asks, and asks only ---
cli '{"name":"set_session","args":{"id":"dream/a","keep_alive":true,"model":"fake/flash-fake","cache_tier":"2s"}}' >/dev/null
cli '{"name":"send_message","args":{"session":"dream/a","message":"the source says hello"}}' >/dev/null
notice="select m.content from messages m join sessions s on m.session_id = s.id where s.name = 'dream/a' and m.content like '%keepalive notice%'"
poll "$notice" "keepalive notice"
ok "(a) the cache window asks the session" $?
[[ -z "$(q "select name from sessions where name like 'dream/a/dream/%'")" ]]
ok "(a) no dreamer is forked by the server" $?
[[ "$(q "$notice" | head -1)" == *"@skill(dreamer)"* ]]
ok "(a) the notice points at the dreamer skill" $?
[[ "$(q "$notice" | head -1)" == *"@skill(handoff)"* ]]
ok "(a) and at the handoff skill" $?
[[ "$(q "select count(*) from events where kind = 'keepalive_notice' and session_id = (select id from sessions where name = 'dream/a')")" -ge 1 ]]
ok "(a) the notice is an event row" $?

# --- (b) mute is explicit, and any message clears it ---
cli '{"name":"mute_keepalive","args":{"session":"dream/a","mute":true}}' >/dev/null
before=$(q "select count(*) from messages m join sessions s on m.session_id = s.id where s.name = 'dream/a' and m.content like '%keepalive notice%'")
sleep 4
[[ "$(q "select count(*) from messages m join sessions s on m.session_id = s.id where s.name = 'dream/a' and m.content like '%keepalive notice%'")" == "$before" ]]
ok "(b) a muted session is quiet" $?
rows_before=$(q "select count(*) from messages m join sessions s on m.session_id = s.id where s.name = 'dream/a' and m.content like '%context only%'")
tasks_before=$(q "select count(*) from pending_tasks where session = 'dream/a' and kind = 'turn'")
cli '{"name":"send_message","args":{"session":"dream/a","message":"context only","continue":false}}' >/dev/null
rows_after=$(q "select count(*) from messages m join sessions s on m.session_id = s.id where s.name = 'dream/a' and m.content like '%context only%'")
tasks_after=$(q "select count(*) from pending_tasks where session = 'dream/a' and kind = 'turn'")
[[ "$rows_after" -gt "$rows_before" && "$tasks_after" == "$tasks_before" ]]
ok "(b) continue=false writes context without opening a turn" $?
[[ -z "$(q "select keepalive_handoff_at from sessions where name = 'dream/a' and keepalive_handoff_at is not null")" ]]
ok "(b) the message cleared the mute" $?
cli '{"name":"send_message","args":{"session":"dream/a","message":"a new prompt"}}' >/dev/null
now=$before
for _ in $(seq 1 150); do
  now=$(q "select count(*) from messages m join sessions s on m.session_id = s.id where s.name = 'dream/a' and m.content like '%keepalive notice%'")
  [[ "$now" -gt "$before" ]] && break
  sleep 0.2
done
[[ "$now" -gt "$before" ]]
ok "(b) the next turn end arms a fresh timer" $?

# --- (e) the startup sweep discards an orphan whose discard was lost to exit ---
stop_server || exit 1
python - "$home" "$(native "$root")" <<'PY'
import sqlite3, sys, time
home, cwd = sys.argv[1], sys.argv[2]
c = sqlite3.connect(f"{home}/gongga.db")
now = int(time.time() * 1000)
source = c.execute("select id from sessions where name = 'dream/a'").fetchone()[0]
c.execute(
    "insert into sessions (name, status, model, cwd, created_at, updated_at, dream_of, dreamed_checkpoint_id)"
    " values ('dream/a/dream/0', 'idle', 'fake/flash-fake', ?, ?, ?, ?, 0)",
    (cwd, now, now, source),
)
c.commit()
PY
start_server || exit 1
poll_gone "select name from sessions where name = 'dream/a/dream/0'"
ok "(e) the startup sweep discards an orphan dream" $?
[[ -z "$(q "select dreamed_checkpoint_id from sessions where name = 'dream/a'")" ]]
ok "(e) the source may dream that generation again" $?
[[ -n "$(q "select kind from events where kind = 'dream_sweep'")" ]]
ok "(e) the sweep is an event row" $?
stop_server || exit 1

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
elapsed=$(( $(date +%s) - started ))
if [[ $fails == 0 ]]; then
  printf 'PASS · dream · %ss · %s\n' "$elapsed" "$sha"
else
  printf 'FAIL · dream · %d scenario(s) · %ss · %s\n' "$fails" "$elapsed" "$sha"; exit 1
fi