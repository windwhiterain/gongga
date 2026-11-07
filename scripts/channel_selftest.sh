#!/usr/bin/env bash
# SPEC's `new` and `check`, on real processes: a channel is a program a relay keeps,
# its output is a window nobody has read yet, and `[channel.*]` is what frames a reply.
#
# One line of verdict at the end; `-v` prints every answer.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/channel-home"
log="$work/channel.log"
verbose=${1:-}
began=$(date +%s)

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
# The server speaks UDP: a TCP dial would never answer, so readiness is the line it
# logs when the socket is up.
wait_listening() {
  for _ in $(seq 1 300); do
    grep -q "listening (UDP) local=127.0.0.1:$1" "$log" && return 0
    sleep 0.2
  done
  return 1
}
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }

# the pids a run leaves: the server, and every channel's own program
channel_pids() {
  for status in "$home"/channels/*/status; do
    [[ -f "$status" ]] || continue
    python -c 'import json,sys
try: print(json.load(open(sys.argv[1]))["pid"])
except Exception: pass' "$status"
  done
}

port=$(free_port)
fake_port=$(free_port)
server_pid=""
fake_pid=""
# Anything still serving this home, by the path it was told to serve.
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
cleanup() {
  for pid in $(channel_pids) "$server_pid" "$fake_pid" $(serving_home); do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  rm -rf "$home" 2>/dev/null || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$work" "$home"
: > "$log"
export GONGGA_HOME="$home"

# A channel that ends is news to its session, and news starts a turn: the turn needs a
# model that answers, or the retries end the server and the run means nothing.
export FAKE_LLM_DUMP="$work/channel-dump.log"
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/channel-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/channel-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/channel-fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/channel-fake.pid" 2>/dev/null)
for _ in $(seq 1 100); do
  python -c 'import socket,sys
s=socket.socket();s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$fake_port" && break
  sleep 0.2
done

cat > "$work/channel-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "CHANNEL_SELFTEST_KEY"

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
export GONGGA_PROVIDERS_CATALOG="$work/channel-roster.toml"
export CHANNEL_SELFTEST_KEY=sk-fake
# A session's shell exports these; a test roster cannot honour them.
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
    -u GONGGA_API_KEY_ENV -u GONGGA_SESSION \
    "$server_bin" --home "$home" --bind "127.0.0.1:$port" \
    --config "$root/ship/config/config.toml" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
server_pid=$(child_pid)
wait_listening "$port" || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }

cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# One field of the answer, written with no trailing newline: Python on Windows ends a
# `print` with `\r\n`, and `$( )` strips only the `\n` -- which leaves a `\r` that makes
# every comparison fail against the text it looks like.
field() {
  python -c 'import json,sys
text, key = sys.argv[1], sys.argv[2]
value = ""
for line in text.splitlines():
    if "cmd_result" not in line:
        continue
    message = json.loads(line)["v"]["event"].get("message", "")
    try:
        value = json.loads(message).get(key, "")
    except Exception:
        value = ""
    break
sys.stdout.write(str(value))' "$1" "$2"
}

"${cli[@]}" set_session main --model fake/flash-fake --cwd "$root" >/dev/null 2>&1

# 1. A program started as a channel, and the name it answers by.
started=$("${cli[@]}" new main bash 2>&1)
say "$started"
shell_id=$(field "$started" id)
ok "new main bash opens a channel" "$([[ -n "$shell_id" ]] && echo 0 || echo 1)"
say "id=$shell_id"

# 2. One command, framed by `[channel.bash]`: output and status, marker gone.
one=$("${cli[@]}" check main "$shell_id" --input 'echo one' --wait 5000 2>&1)
say "$one"
say "parsed: matched=[$(field "$one" matched)] exit=[$(field "$one" exit)] output=[$(field "$one" output)]"
ok "check --input frames the reply" "$([[ "$(field "$one" matched)" == True && "$(field "$one" output | tr -d '\n')" == one ]] && echo 0 || echo 1)"
ok "the sentinel's tail is the status" "$([[ "$(field "$one" exit)" == 0 ]] && echo 0 || echo 1)"
ok "the marker is not in the window" "$(has "$one" '__gongga' && echo 1 || echo 0)"

# 3. A failing command says so, and the channel is not ended by it.
bad=$("${cli[@]}" check main "$shell_id" --input 'nosuchcommand-here' --wait 5000 2>&1)
say "$bad"
ok "a failing command answers 127" "$([[ "$(field "$bad" exit)" == 127 ]] && echo 0 || echo 1)"
third=$("${cli[@]}" check main "$shell_id" --input 'echo three' --wait 5000 2>&1)
ok "the channel still answers after a failure" "$([[ "$(field "$third" output | tr -d '\n')" == three ]] && echo 0 || echo 1)"

# 4. The window moves: what one call took, the next does not see.
empty=$("${cli[@]}" check main "$shell_id" --wait 0 2>&1)
say "$empty"
say "parsed: output=[$(field "$empty" output)]"
ok "check --wait 0 reads an empty window" "$([[ -z "$(field "$empty" output | tr -d '\n')" ]] && echo 0 || echo 1)"

# 5. A one-shot program: it ends, and `exit` is its own.
once=$("${cli[@]}" new main bash --args '["-lc", "echo done; exit 3"]' --wait 8000 2>&1)
say "$once"
say "parsed: exit=[$(field "$once" exit)] output=[$(field "$once" output)]"
oneshot_exit=$(field "$once" exit)
oneshot_out=$(field "$once" output)
say "one-shot: exit=[$oneshot_exit] out=[$oneshot_out]"
ok "a one-shot channel reports its own exit" "$([[ "$oneshot_exit" == 3 && "$oneshot_out" == done ]] && echo 0 || echo 1)"

# 6. A cmd through the same door: `GONGGA_CLI` with the caller's session filled in.
cloned=$("${cli[@]}" new main GONGGA_CLI --args '{"cmd": "clone_session", "name": "copy"}' 2>&1)
say "$cloned"
ok "GONGGA_CLI routes to the command layer" "$(has "$cloned" '"ok":true' && echo 0 || echo 1)"
copied=$("${cli[@]}" get_messages --session copy 2>&1)
ok "the cmd really ran" "$(has "$copied" 'last_seq' && echo 0 || echo 1)"

# 7. ls_job lists what the session has, and del_job ends one.
listed=$("${cli[@]}" ls_job --session main "" 2>&1)
say "$listed"
ok "ls_job lists the channels" "$(has "$listed" "$shell_id" && echo 0 || echo 1)"
ended=$("${cli[@]}" del_job --session main "$shell_id" 2>&1)
ok "del_job ends a channel" "$(has "$ended" 'ended channel' && echo 0 || echo 1)"

# 8. A subscription's action runs in a channel of the session's, twice in the same one.
armed=$(GONGGA_SESSION=main "${cli[@]}" subscribe watcher '^channel_selftest$' \
  --persistant true --bash plug --cmd "echo sub-ran" 2>&1)
ok "a subscription arms with a channel" "$(has "$armed" '"ok":true' && echo 0 || echo 1)"
"${cli[@]}" trigger_event channel_selftest >/dev/null 2>&1
sleep 1
"${cli[@]}" trigger_event channel_selftest >/dev/null 2>&1
sleep 2
twice=$(cat "$home"/channels/*/out 2>/dev/null | grep -c '^sub-ran$')
plug=$(ls -d "$home"/channels/*/ 2>/dev/null | wc -l)
ok "both matches wrote into one channel" "$([[ "$twice" == 2 ]] && echo 0 || echo 1)"
say "channels on disk: $plug; sub-ran lines: $twice"

took=$(( $(date +%s) - began ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s\n' "$took" "$sha"
else
  printf 'FAIL · %ss · %s · %s check(s)\n' "$took" "$sha" "$fails"
  exit 1
fi
