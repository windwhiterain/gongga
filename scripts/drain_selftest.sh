#!/usr/bin/env bash
# `cancel` interrupts every turn and leaves. A turn inside a long bash call must
# not hold the process, and nothing unfinished may be owed to the next start.
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
# A start can wait on the community roster's fetch before it binds; a slow
# network is not a server that never listened.
wait_tcp() { for _ in $(seq 1 300); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
fresh_home() {
  # `rm -rf`, not shutil.rmtree: a copied .git holds read-only objects, and
  # ignore_errors left them behind for the next run's `cp -r` to nest inside.
  rm -rf "$1" || echo "note: $1 is still held by a process" >&2
  mkdir -p "$1"
}

port=$(free_port); fake_port=$(free_port); fake_pid=""
cleanup() {
  for pid in $(serving_home); do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/drain.pid"
  sleep 1
  fresh_home "$home"
  rmdir "$home" 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p "$work"
fresh_home "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"

# A tool call far longer than the window cancel is given to leave in.
export FAKE_LLM_DUMP="$dump"
export FAKE_LLM_TOOL=bash
export FAKE_LLM_TOOL_ARGS='{"command":"sleep 60","name":"default","timeout_ms":120000}'
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

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; exit 1; }

cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")
created=$("${cli[@]}" set_session sleeper fake/flash-fake --cwd "$work" 2>&1)
[[ "$created" == *'"ok":true'* ]] || { echo "FAIL · session not created: $created" >&2; exit 1; }

# The payload leads: `--message <text> --name <session>` is the declared order.
"${cli[@]}" send_message --message "run the slow command" --name sleeper >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q "tool start session=sleeper" "$log" && break; sleep 0.2; done
grep -q "tool start session=sleeper" "$log"
ok "a turn is inside a 60s bash call" $?
[[ $fails == 0 ]] || { echo "FAIL · the turn never started"; exit 1; }

cancelled=$("${cli[@]}" cancel 2>&1)
[[ "$cancelled" == *'"ok":true'* ]]
ok "cancel is acknowledged" $?

grep -q "every turn was interrupted" "$log"
ok "the stop interrupts turns, not a boundary flag" $?

# The 60s call is still out there. A draining server would wait for it; this
# one must be gone within ten seconds.
for _ in $(seq 1 100); do grep -q "server stopped" "$log" && break; sleep 0.1; done
grep -q "server stopped" "$log"
ok "the server stops without waiting for the long call" $?

pid=$(cat "$home/server.lock" 2>/dev/null)
for _ in $(seq 1 50); do alive "$pid" || break; sleep 0.1; done
! alive "$pid"
ok "the process is gone" $?

owed=$(python -c 'import sqlite3,sys
con=sqlite3.connect(sys.argv[1],timeout=0.5)
print(con.execute("select count(*) from pending_tasks").fetchone()[0])
con.close()' "$home/gongga.db" 2>/dev/null || echo unknown)
[[ "$owed" == "0" ]]
ok "nothing is left for the next start (pending=$owed)" $?

# The ask, not the jobs: a client `cancel` lets them go and kills none.
stop=$(python -c 'import sqlite3,sys
con=sqlite3.connect(sys.argv[1],timeout=0.5)
row=con.execute("select payload from events where kind=? order by id desc limit 1",("server_stop",)).fetchone()
con.close()
print(row[0] if row else "")' "$home/gongga.db" 2>/dev/null || echo "")
[[ "$stop" == *'"killed":0'* && "$stop" == *'"released":'* ]]
ok "a client cancel lets jobs go and kills none" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · cancel interrupts · %s\n' "$sha"
else printf 'FAIL · cancel interrupts · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1; fi
