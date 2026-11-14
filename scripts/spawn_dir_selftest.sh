#!/usr/bin/env bash
# A directory this machine does not have, on real processes. A session whose own
# directory is on a peer, or is gone, must still be able to run a skill's entry and
# start a channel -- in this server's own directory -- and `exec`, which falls back to
# the session's directory, must refuse by naming that directory and never the program.
#
# One line of verdict at the end; `-v` keeps the scratch and prints each turn.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and the server reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
tag="spawn-dir-$$"
work="$root/.dev/selftest/$tag"
home="$work/home"
log="$work/server.log"
dump="$work/requests.jsonl"
verbose=${1:-}

say() { [[ "$verbose" == -v ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
kind() { [[ "$1" == *"$2"* ]] && echo 0 || echo 1; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build" >&2; exit 1; }
server_bin="$bin/gongga-server"
[[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"

free_port() {
  python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}
wait_listening() {
  for _ in $(seq 1 300); do
    grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0
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
server_pid=""
fake_pid=""
# The pid of a server is the one holding its home's lock, never `$!`.
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# Every process serving THIS home, whatever a failed run remembered.
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
cleanup() {
  for pid in $(serving_home) "$server_pid" "$fake_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 2
  [[ "$verbose" == -v ]] && return 0
  rm -rf "$home" 2>/dev/null || { sleep 3; rm -rf "$home"; } \
    || echo "note: $home is still held by a process" >&2
  rm -rf "$work"
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$work" "$home"
# This machine's own content: a home with none reaches for the network.
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null

# A skill of this run's own: an entry that says where it stands, so the answer is the
# directory rather than a shape.
mkdir -p "$home/skills/echo"
cat > "$home/skills/echo/SKILL.md" <<'MD'
---
name: echo
description: Print the directory this entry stands in and the words it was given.
metadata:
  entry: echo.sh
---

# echo

A selftest's skill: it prints its directory and its words.
MD
cat > "$home/skills/echo/echo.sh" <<'SHENTRY'
#!/usr/bin/env bash
printf 'dir=%s\n' "$(pwd -W 2>/dev/null || pwd)"
printf 'words=%s\n' "$*"
SHENTRY

# A peer nothing contacts: these scenarios only need a session whose directory is on
# another machine, which is what leaves its own directory empty here.
cat > "$work/remotes.toml" <<'TOML'
[remotes.probe-vm]
host = "127.0.0.1"
port = 1
TOML
export GONGGA_REMOTES_CATALOG="$work/remotes.toml"
export GONGGA_HOME="$home"
export FAKE_LLM_DUMP="$dump"
port=$(free_port)
fake_port=$(free_port)
cat > "$work/roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "SPAWN_DIR_KEY"

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
export SPAWN_DIR_KEY=sk-fake
: > "$log"; : > "$dump"

# The stand-in model, restarted on its own port between turns: one turn is the unit
# that needs a different tool.
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
cd "$root" || exit 1
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
    -u GONGGA_API_KEY_ENV -u GONGGA_SESSION \
    "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
server_pid=$(child_pid)
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")
call() { "${cli[@]}" "$1" 2>&1; }

# A turn is counted, not timed: the stand-in answers in milliseconds.
turn_of() { grep -c "turn end session=$1" "$log" 2>/dev/null; }
wait_turn() {
  local want=$(( $(turn_of "$1") + 1 ))
  for _ in $(seq 1 240); do [[ "$(turn_of "$1")" -ge "$want" ]] && return 0; sleep 0.5; done
  return 1
}
# What one tool answered in the newest tool result of that session.
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

model='"model_config":{"model":{"provider":"fake","model":"flash-fake"}}'

# 1. The reported bug: a session whose directory is on a peer has none here, and a
#    skill's entry still runs -- in this server's directory, not in a failed spawn.
out=$(call '{"name":"new_session","args":{"id":"probe","config":{'"$model"',"workspace":{"remote":"probe-vm"}}}}')
say "new_session probe: $out"
out=$(call '{"name":"run_skill","args":{"id":"harness/echo","cmd":{"args":["hi"]},"session":"probe"}}')
say "run_skill on probe: $(printf '%s' "$out" | tail -2)"
ok "a peer session's entry skill runs here" "$(kind "$out" '[exit status 0]')"
ok "and stands in this server's directory" "$(kind "$out" "dir=$(native "$root")")"
ok "with the words it was given" "$(kind "$out" 'words=hi')"
ok "not the empty directory it failed on" "$([[ "$out" != *'os error'* ]] && echo 0 || echo 1)"

# 2. The same for a session whose own directory this machine no longer has.
mkdir -p "$work/gone"
call '{"name":"new_session","args":{"id":"stale","config":{'"$model"',"workspace":{"cwd":"'"$(native "$work/gone")"'"}}}}' >/dev/null
rm -rf "$work/gone"
out=$(call '{"name":"run_skill","args":{"id":"harness/echo","cmd":{"args":["gone"]},"session":"stale"}}')
say "run_skill on stale: $(printf '%s' "$out" | tail -2)"
ok "a gone directory is not a failed entry" "$(kind "$out" '[exit status 0]')"
ok "and the entry still stands in this server's directory" "$(kind "$out" "dir=$(native "$root")")"

# 3. A channel on this machine: `new_channel` uses the session's directory directly.
out=$(call '{"name":"new_channel","args":{"id":"/bash/c","name":"bash","session":"stale"}}')
say "new_channel on stale: $out"
ok "a channel starts for a session whose directory is gone" "$(kind "$out" 'pid')"
out=$(call '{"name":"input_channel","args":{"id":"/bash/c","input":"pwd -W","session":"stale","wait":8000}}')
ok "and stands in this server's directory" "$(kind "$out" "$(native "$root")")"

# 4. `exec` falls back to the session's directory, so it must say which directory it
#    could not use -- never that the program could not run.
fake_up exec '{"bin":"bash","args":["-c","echo hi"],"remote":"none","cwd":"'"$(native "$work/nope")"'","wait":3000}'
call '{"name":"send_message","args":{"message":"a stated directory that is gone","session":"stale"}}' >/dev/null
wait_turn stale
answer=$(last_tool stale)
say "exec with a stated cwd: $answer"
ok "a stated directory that is gone is refused by name" \
  "$(kind "$answer" "no directory \"$(native "$work/nope")\"")"
ok "and the program is not blamed for it" "$([[ "$answer" != *'could not run'* ]] && echo 0 || echo 1)"

fake_up exec '{"bin":"bash","args":["-c","echo hi"],"remote":"none","wait":3000}'
call '{"name":"send_message","args":{"message":"the sessions own directory is gone","session":"stale"}}' >/dev/null
wait_turn stale
answer=$(last_tool stale)
say "exec with the session's cwd: $answer"
ok "the session's own directory is named as the session's" \
  "$([[ "$answer" == *"this session's directory"* && "$answer" == *"gone"* ]] && echo 0 || echo 1)"
ok "and the program is not blamed for it either" "$([[ "$answer" != *'could not run'* ]] && echo 0 || echo 1)"

# 5. A peer session with no directory at all still runs a program here.
fake_up exec '{"bin":"bash","args":["-c","echo alive"],"remote":"none","wait":4000}'
call '{"name":"send_message","args":{"message":"run something here","session":"probe"}}' >/dev/null
wait_turn probe
answer=$(last_tool probe)
say "exec on probe: $answer"
ok "a session with no directory here still runs a program" "$(kind "$answer" 'alive')"
ok "and nothing was refused over a directory" \
  "$([[ "$answer" != *'os error'* && "$answer" != *'no directory'* ]] && echo 0 || echo 1)"

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · spawn-dir · %s\n' "$sha"
else
  printf 'FAIL · spawn-dir · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
