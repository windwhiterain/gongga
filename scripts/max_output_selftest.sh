#!/usr/bin/env bash
# One cap for every tool call, on real turns: a stand-in model asks for a tool call,
# the server runs it, and this reads the answer the model is actually handed.
#
# Why real turns are the only proof: every defect this pins was invisible from the
# source. `max_output` above the home's default was silently lowered to it, because a
# tool capped as well and the smaller number won; the cap counted bytes while the
# notice named characters; over the cap, the file the answer pointed at held the
# answer's own cut instead of the output; and a call routed as a command had
# `max_output` taken off its arguments and then dropped.
#
# Shape from scripts/tool_selftest.sh: its own home, its own port, a stand-in model on
# its own port, a trap that kills the whole tree, one verdict line.
set -uo pipefail

native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$root/scripts/selftest_bin.sh"
bin=$(selftest_bin "$root") || exit 1
home="$root/.dev/cap-home"
log="$root/.dev/cap-home.log"
dump="$root/.dev/cap-home.requests"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

server_bin="$bin/gongga-server"
[[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"
relay_bin="$bin/gongga-relay"
[[ -x "$relay_bin" ]] || relay_bin="$bin/gongga-relay.exe"

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
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
wait_tcp() { for _ in $(seq 1 100); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }

port=$(free_port)
fake_port=$(free_port)
server_pid=""
fake_pid=""

# Every process whose command line names this home: the server AND its watchdog, which
# is the same binary with the same arguments watching itself. Killing only the process
# that answers is not a kill -- the watchdog restarts it, and a home removed underneath
# a restarting server leaves it failing to write its own lock. So this kills by home,
# tree-kill, and repeats until nothing is left.
home_pids() {
    local posix win
    posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
    win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
    # Matched by NAME as well as by home: the probe that asks this question has the
    # home in its own command line, so a match on the path alone finds itself and the
    # loop never empties. Only a server or its relay belongs to a home.
    powershell -NoProfile -Command \
        "Get-CimInstance Win32_Process | Where-Object { (\$_.Name -eq 'gongga-server.exe' -or \$_.Name -eq 'gongga-relay.exe') -and (\$_.CommandLine -like '*$posix*' -or \$_.CommandLine -like '*$win*') } | ForEach-Object { \$_.ProcessId }" \
        2>/dev/null | tr -d '\r' | tr '\n' ' '
}
kill_home() {
    local pids n=0
    for _ in $(seq 1 10); do
        pids=$(home_pids)
        [[ -z "${pids// /}" ]] && return 0
        n=$((n + 1))
        # -T first: the tree goes, so nothing is left to respawn what was killed.
        for pid in $pids; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
        sleep 1
    done
    echo "note: still holding $home after $n rounds: $(home_pids)" >&2
    return 1
}
cleanup() {
    [[ -n "${fake_pid:-}" ]] && alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
    kill_home || return 0
    if [[ "${KEEP:-0}" == 1 ]]; then
        echo "note: kept $home (KEEP=1)" >&2
        return 0
    fi
    rm -rf "$home" 2>/dev/null || echo "note: $home could not be removed" >&2
    rm -f "$dump" 2>/dev/null
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$home"
: > "$log"; : > "$dump"
# A leftover from an earlier run would fight this one for the home and the port.
kill_home || { echo "a previous run still holds $home" >&2; exit 1; }

[[ -x "$relay_bin" ]] || { echo "no gongga-relay beside the server: cargo build -p gongga-relay" >&2; exit 1; }

cat > "$home/roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "CAP_SELFTEST_KEY"

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

export GONGGA_HOME="$home"
export GONGGA_PROVIDERS_CATALOG="$home/roster.toml"
export CAP_SELFTEST_KEY=sk-fake
export GONGGA_SHIP_URL="${GONGGA_SHIP_URL:-$root/ship}"
export FAKE_LLM_DUMP="$dump"

# The stand-in is restarted on the same port between turns: `FAKE_LLM_TOOL` is its
# environment, and one turn is the unit that needs a different call.
fake_up() {
    if [[ -n "$fake_pid" ]]; then
        alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
        for _ in $(seq 1 50); do
            dial_ready "$fake_port" || break
            sleep 0.2
        done
    fi
    rm -f "$home/fake.pid"
    env FAKE_LLM_TOOL="$1" FAKE_LLM_TOOL_ARGS="$2" \
        python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
        "$home/fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$home/fake.log" 2>&1 &
    for _ in $(seq 1 50); do [[ -s "$home/fake.pid" ]] && break; sleep 0.2; done
    fake_pid=$(cat "$home/fake.pid" 2>/dev/null)
    wait_tcp "$fake_port"
}

# The call the stand-in asks for. `exec` needs an `id` and an `input`; the character
# run is written by a program so the answer is exactly as long as the scenario says.
call_exec() { # id program max_output -> FAKE_LLM_TOOL_ARGS JSON
    python - "$@" <<'PY'
import json, sys
cid, text, max_output = sys.argv[1:4]
# No `session`: inside a turn the call belongs to that session already, and the
# schema refuses an argument the tool does not take.
# The marker ends the wait as soon as the output is complete, instead of sitting out
# the call's whole `wait`. `input` is one line of bash, so the marker is a command
# after it on that line.
args = {"id": cid, "input": text + "; echo ZZDONE", "wait": 20000, "until": "ZZDONE"}
if max_output != "-":
    args["max_output"] = int(max_output)
print(json.dumps(args))
PY
}

fake_up "" "{}" || { echo "the stand-in never answered on 127.0.0.1:$fake_port" >&2; exit 1; }

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
    -u GONGGA_API_KEY_ENV -u GONGGA_SESSION \
    "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 200); do
    grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && [[ -s "$home/server.lock" ]] && break
    sleep 0.2
done
[[ -s "$home/server.lock" ]] || { echo "the server never listened on 127.0.0.1:$port" >&2; sed -n 1,40p "$log"; exit 1; }
server_pid=$(child_pid)
for _ in $(seq 1 50); do [[ -x "$home/bin/gongga-cli" ]] && break; sleep 0.2; done
[[ -x "$home/bin/gongga-cli" ]] || { echo "the server wrote no bin/gongga-cli" >&2; tail -20 "$log"; exit 1; }
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# A turn is counted, not timed: the stand-in answers in milliseconds, so a count taken
# after `send_message` returns can already include the turn it is waiting for.
turns=0
wait_turn() {
    turns=$((turns + 1))
    for _ in $(seq 1 240); do
        local got
        got=$(python - "$home" "$1" <<'PY'
import sqlite3, sys
try:
    c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
    row = c.execute("select id from sessions where name=?", (sys.argv[2],)).fetchone()
    print(0 if row is None else c.execute(
        "select count(*) from messages where session_id=? and kind='tool_result'",
        (row[0],)).fetchone()[0])
except Exception:
    print(0)
PY
)
        [[ "${got:-0}" -ge "$turns" ]] && return 0
        sleep 0.5
    done
    return 1
}

# What the newest tool answer says: the output inside the `exec` envelope, the length
# of that output in characters, and the file an over-cap answer names.
answer() {
    python - "$home" "$1" <<'PY'
import json, re, sqlite3, sys
home, session = sys.argv[1], sys.argv[2]
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
row = c.execute("select id from sessions where name=?", (session,)).fetchone()
out = {"chars": 0, "path": None, "notice": False}
if row is not None:
    for (display,) in c.execute(
            "select display from messages where session_id=? and kind='tool_result' "
            "order by seq desc limit 1", (row[0],)):
        text = json.loads(display).get("output", "")
        # An over-cap answer is not the envelope any more: the cap replaces it with the
        # head, a notice, and the tail. The notice names the file; nothing else does.
        named = re.search(r"written to (\S+)", text)
        omitted = re.search(r"\[(?:\.\.\.|…) ?(\d+) characters omitted", text)
        claimed = re.search(r"full output \((\d+) characters\)", text)
        out["notice"] = "omitted" in text
        out["path"] = named.group(1) if named else None
        out["omitted"] = int(omitted.group(1)) if omitted else None
        out["claimed_total"] = int(claimed.group(1)) if claimed else None
        try:
            envelope = json.loads(text)
            window = envelope.get("output", "")
            out["chars"] = len(window)
        except Exception:
            # Over the cap the envelope is replaced by the head, the notice and the tail:
            # what the model reads is that text, and that is the length a cap governs.
            out["chars"] = len(text)
print(json.dumps(out))
PY
}

# One scenario: restart the stand-in so its next turn asks for `call`, run a turn, and
# let the caller read the newest answer.
run_turn() { # tool session-name call-args
    local tool=$1 name=$2 args=$3
    fake_up "$tool" "$args"
    "${cli[@]}" '{"name":"send_message","args":{"session":"'"$name"'","message":"go"}}' >/dev/null 2>&1
    wait_turn "$name"
}

created=$("${cli[@]}" '{"name":"set_session","args":{"id":"cap","model":"fake/flash-fake","cwd":"'"$(native "$root")"'"}}' 2>&1)
[[ "$created" == *'"ok":true'* ]]; ok "a session to call from" $?

# The channel one program, reused by every scenario: the cap is what varies.
started=$("${cli[@]}" '{"name":"exec","args":{"session":"cap","bin":"bash","id":"cap/shell","wait":15000}}' 2>&1)
[[ "$started" == *'"ok":true'* ]]; ok "a shell channel" $?

chars_of() { python -c 'import json,sys;print(json.loads(sys.argv[1])["chars"])' "$1" 2>/dev/null || echo 0; }

# --- 1. A call's number RAISES the answer above the home's default -----------
# 12000 characters: over the home's 8000, under the call's 20000. Before the change the
# tool's own 8000 won, and the answer came back at about 8000 characters.
run_turn exec cap "$(call_exec cap/shell "python -c \"print(chr(67)*12000)\"" 20000)"
saw=$(answer cap)
say "$saw"
chars=$(chars_of "$saw")
ok "max_output above the default raises the answer ($chars of 12000 characters)" "$([[ $chars -ge 11000 ]] && echo 0 || echo 1)"

# --- 2. The cap counts characters, not bytes --------------------------------
# 4000 CJK characters are 12045 bytes but only 4000 characters: under 8000, so nothing
# may be cut. Counting bytes cut this to about 2651 characters.
run_turn exec cap "$(call_exec cap/shell "python -c \"import sys; sys.stdout.write(chr(0x4e2d)*4000)\"" -)"
saw=$(answer cap)
say "$saw"
chars=$(chars_of "$saw")
ok "the cap counts characters, not bytes ($chars of 4000 CJK characters kept)" "$([[ $chars -ge 3900 ]] && echo 0 || echo 1)"

# --- 3. Over the cap, the answer names a file that holds the WHOLE output ----
# The call is capped at 1000, and the file it names must still carry all 12000.
run_turn exec cap "$(call_exec cap/shell "python -c \"print(chr(68)*12000)\"" 1000)"
saw=$(answer cap)
say "$saw"
chars=$(chars_of "$saw")
path=$(python -c 'import json,sys;print(json.loads(sys.argv[1]).get("path") or "")' "$saw" 2>/dev/null || echo "")
ok "an over-cap answer is cut to its stated length ($chars characters)" "$([[ $chars -gt 0 && $chars -le 1600 ]] && echo 0 || echo 1)"
omitted=$(python -c 'import json,sys;print(json.loads(sys.argv[1]).get("omitted") or 0)' "$saw" 2>/dev/null || echo 0)
claimed=$(python -c 'import json,sys;print(json.loads(sys.argv[1]).get("claimed_total") or 0)' "$saw" 2>/dev/null || echo 0)
# The defect this pins: the notice used to describe a file that was itself cut, so its
# own numbers were of the truncated text. `claimed - omitted` is how much of the real
# answer survived, and with a cap of 1000 that has to BE 1000. The rest of `chars` is
# the notice's own prose, which is why the raw length is not the thing to assert.
kept=$((claimed - omitted))
ok "the cap governs what survives, and the notice says so (${claimed} - ${omitted} = ${kept}, cap 1000)" \
    "$([[ $kept -eq 1000 ]] && echo 0 || echo 1)"
if [[ -n "$path" ]]; then
    held=$(python - "$path" <<'PY'
import sys
try:
    text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
except Exception:
    print(0); raise SystemExit(0)
print(text.count("D"))
PY
)
    ok "the named file holds the whole output ($held of 12000)" "$([[ $held -ge 11000 ]] && echo 0 || echo 1)"
else
    ok "the named file holds the whole output (no file named)" 1
fi

# --- 4. A call routed as a command is held to the cap too -------------------
# `ls_channel` is a command, not a tool row: its `max_output` used to be taken off the
# arguments and dropped, because the command branch returned before the cap was read.
run_turn ls_channel cap '{"max_output":1000}'
saw=$(answer cap)
say "$saw"
chars=$(chars_of "$saw")
ok "a command call is held to max_output ($chars characters under 1000)" "$([[ $chars -gt 0 && $chars -le 1200 ]] && echo 0 || echo 1)"

printf -- '---\n'
if [[ "$fails" == 0 ]]; then
    printf 'PASS - one cap, every call - %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    exit 0
else
    printf 'FAIL - %d check(s) - %s\n' "$fails" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    exit 1
fi