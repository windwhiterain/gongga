#!/usr/bin/env bash
# SPEC's `exec`, on real processes: a channel is a program a relay keeps,
# its output is a window nobody has read yet, and `[channel.*]` is what frames a reply.
#
# One line of verdict at the end; `-v` prints every answer.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and a Windows program -- the
# server is one -- reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
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
    --log info >>"$log" 2>&1 &
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

"${cli[@]}" '{"name":"set_session","args":{"id":"main","model":"fake/flash-fake","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1

# 1. A program started as a channel, and the name it answers by.
started=$("${cli[@]}" '{"name":"exec","args":{"session":"main","bin":"bash"}}' 2>&1)
say "$started"
shell_id=$(field "$started" id)
ok "exec bash opens a channel" "$([[ -n "$shell_id" ]] && echo 0 || echo 1)"
say "id=$shell_id"

# 2. One command, framed by `[channel.bash]`: output and status, marker gone.
one=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"echo one","wait":5000}}' 2>&1)
say "$one"
say "parsed: matched=[$(field "$one" matched)] exit=[$(field "$one" exit)] output=[$(field "$one" output)]"
ok "exec --input frames the reply" "$([[ "$(field "$one" matched)" == True && "$(field "$one" output | tr -d '\n')" == one ]] && echo 0 || echo 1)"
ok "the sentinel's tail is the status" "$([[ "$(field "$one" exit)" == 0 ]] && echo 0 || echo 1)"
ok "the marker is not in the window" "$(has "$one" '__gongga' && echo 1 || echo 0)"

# 3. A failing command says so, and the channel is not ended by it.
bad=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"nosuchcommand-here","wait":5000}}' 2>&1)
say "$bad"
ok "a failing command answers 127" "$([[ "$(field "$bad" exit)" == 127 ]] && echo 0 || echo 1)"
third=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"echo three","wait":5000}}' 2>&1)
ok "the channel still answers after a failure" "$([[ "$(field "$third" output | tr -d '\n')" == three ]] && echo 0 || echo 1)"

# 3b. A command that takes longer than the client's own patience. The server retires
#     a frame only when the peer acknowledges it, and gives up on a connection that
#     never does -- so this is the one scenario where a silent client loses its answer.
slow=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"sleep 2; echo slow-said","wait":15000}}' 2>&1)
say "$slow"
ok "a command slower than a second still answers" \
  "$([[ "$(field "$slow" output | tr -d '\n')" == slow-said ]] && echo 0 || echo 1)"

# 3c. `time` is what this call cost: from its own write, not from when the channel
#     was opened. The channel has been alive for a while by now.
aged=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"echo aged","wait":5000}}' 2>&1)
say "aged: time=$(field "$aged" time)"
ok "time is measured from this call's input"   "$(python -c 'import sys
raw = sys.argv[1].rstrip("s")
print(0 if 0 < float(raw) < 3 else 1)' "$(field "$aged" time)" 2>/dev/null || echo 1)"

# 4. The window moves: what one call took, the next does not see.
empty=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","wait":0}}' 2>&1)
say "$empty"
say "parsed: output=[$(field "$empty" output)]"
ok "exec --wait 0 reads an empty window" "$([[ -z "$(field "$empty" output | tr -d '\n')" ]] && echo 0 || echo 1)"

# 5. A one-shot program: it ends, and `exit` is its own.
once=$("${cli[@]}" '{"name":"exec","args":{"session":"main","bin":"bash","id":"'"$shell_id-oneshot"'","args":["-lc","echo done; exit 3"],"wait":8000}}' 2>&1)
say "$once"
say "parsed: exit=[$(field "$once" exit)] output=[$(field "$once" output)]"
oneshot_exit=$(field "$once" exit)
oneshot_out=$(field "$once" output)
say "one-shot: exit=[$oneshot_exit] out=[$oneshot_out]"
ok "a one-shot channel reports its own exit" "$([[ "$oneshot_exit" == 3 && "$oneshot_out" == done ]] && echo 0 || echo 1)"

# 6. The machine and the directory are the call's, not the session's.
elsewhere=$(mktemp -d 2>/dev/null || echo "$work/elsewhere")
mkdir -p "$elsewhere"
in_dir=$("${cli[@]}" '{"name":"exec","args":{"session":"main","bin":"bash","id":"'"$shell_id-dir"'","cwd":"'"$(native "$elsewhere")"'","args":["-lc","pwd"],"wait":8000}}' 2>&1)
say "$in_dir"
ok "exec --cwd starts the program there" "$([[ "$(field "$in_dir" output)" == *"$(basename "$elsewhere")"* ]] && echo 0 || echo 1)"
rmdir "$elsewhere" 2>/dev/null
refused=$("${cli[@]}" '{"name":"exec","args":{"session":"main","bin":"bash","id":"'"$shell_id-nowhere"'","remote":"nowhere-named"}}' 2>&1)
say "$refused"
ok "a machine that cannot be reached is refused, not run here" "$([[ "$(field "$refused" id)" == "" ]] && has "$refused" 'no remote named' && echo 0 || echo 1)"

# 7. A command is reached by a tool call, not by a channel: SPEC's `fn <tool>(**args)`.
cloned=$("${cli[@]}" '{"name":"clone_session","args":{"id":"main","new_id":"copy"}}' 2>&1)
say "$cloned"
ok "a command runs as itself" "$(has "$cloned" '"ok":true' && echo 0 || echo 1)"
copied=$("${cli[@]}" '{"name":"get_messages","args":{"session":"copy"}}' 2>&1)
ok "the cmd really ran" "$(has "$copied" 'last_seq' && echo 0 || echo 1)"

# 7. ls_channel lists what the session has, and del_channel ends one.
listed=$("${cli[@]}" '{"name":"ls_channel","args":{"session":"main","path":""}}' 2>&1)
say "$listed"
ok "ls_channel lists the channels" "$(has "$listed" "$shell_id" && echo 0 || echo 1)"
ended=$("${cli[@]}" '{"name":"del_channel","args":{"session":"main","id":"'"$shell_id"'"}}' 2>&1)
ok "del_channel ends a channel" "$(has "$ended" 'ended channel' && echo 0 || echo 1)"

# 8. A subscription runs one command per match: the log row is what a match leaves.
armed=$("${cli[@]}" '{"name":"subscribe","args":{"pattern":"^channel_selftest$","cmd":{"name":"trigger_event","args":{"event":"sub_ran"}},"session":"main","id":"watcher","persistant":true}}' 2>&1)
say "$armed"
ok "a subscription carries the command it runs" \
  "$(has "$armed" '"ok":true' && echo 0 || echo 1)"
"${cli[@]}" '{"name":"trigger_event","args":{"event":"channel_selftest"}}' >/dev/null 2>&1
sleep 1
"${cli[@]}" '{"name":"trigger_event","args":{"event":"channel_selftest"}}' >/dev/null 2>&1
sleep 2
ran=$("${cli[@]}" '{"name":"events","args":{"kind":"subscription","session":"main"}}' 2>&1)
twice=$(printf '%s' "$ran" | grep -c 'sub_ran')
ok "both matches ran the command it carries" \
  "$([[ "$twice" == 2 ]] && echo 0 || echo 1)"
say "subscription rows: $(printf '%s' "$ran" | head -c 300)"

took=$(( $(date +%s) - began ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s\n' "$took" "$sha"
else
  printf 'FAIL · %ss · %s · %s check(s)\n' "$took" "$sha" "$fails"
  exit 1
fi
