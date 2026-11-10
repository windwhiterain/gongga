#!/usr/bin/env bash
# A session with a remote has two machines: `remote_bash` there, `bash` here, one
# name space, and every reference read where it was printed. None of that can be
# seen without real processes -- the arrangement, the prompt's index and the tool
# table are all built from what the peer answers -- so this runs a real server
# against the peer named in the operator's own config, with a stand-in model.
#
# One line of verdict at the end; `-v` keeps the scratch and prints each turn.
# The peer is the first `[remotes.<name>]` (or its old `[targets.<name>]` spelling)
# in `~/.gongga/config.toml`, which is also what proves the old spelling still reads.
# Nothing is copied to the peer but one scratch directory, created and removed
# through the session's own `remote_bash`.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/remote-home"
log="$work/remote.log"
dump="$work/remote-requests.jsonl"
verbose=${1:-}
started=$(date +%s)
tag="gongga-selftest-$$"
marker="remote-selftest-$$"

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server (or set GONGGA_BIN)" >&2
  exit 1; }
exe() { [[ -x "$1.exe" ]] && echo "$1.exe" || echo "$1"; }
server_bin=$(exe "$bin/gongga-server")
cli_bin=$(exe "$home/bin/gongga-cli")

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
wait_tcp() { for _ in $(seq 1 150); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }
wait_free() { for _ in $(seq 1 100); do dial_ready "$1" || return 0; sleep 0.2; done; return 1; }
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }
# Every process serving THIS home: the home has several spellings, and matching one
# of them finds nothing.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

# The peer to prove this against: this machine's own remotes file, or -- for a home
# that has not been migrated yet -- the tables still sitting in its `config.toml`.
# Either way the name is copied into a scratch machine root below, so the run never
# reads the operator's file twice and never writes it.
machine_remotes="${GONGGA_AGENTS_HOME:-$HOME/.agents}/remotes.toml"
peer_source=$machine_remotes
[[ -f "$peer_source" ]] || peer_source="$HOME/.gongga/config.toml"
peer=$(python - "$peer_source" <<'PY'
import re, sys
try:
    text = open(sys.argv[1], encoding="utf-8").read()
except OSError:
    sys.exit(2)
heads = [(m.start(), m.group(1)) for m in re.finditer(r"^\[(?:remotes|targets)\.([^\]]+)\]", text, re.M)]
if not heads:
    sys.exit(2)
start, name = heads[0]
body = []
for line in text[start:].splitlines()[1:]:
    if line.startswith("["):
        break
    body.append(line)
fields = dict(
    (k.strip(), v.strip().strip('"'))
    for k, v in (line.split("=", 1) for line in body if "=" in line and not line.strip().startswith("#"))
)
print(name)
print(fields.get("host", ""))
print(fields.get("identity", ""))
print(fields.get("port", ""))
print(fields.get("runner", ""))
PY
)
peer_name=$(sed -n 1p <<<"$peer")
peer_host=$(sed -n 2p <<<"$peer")
peer_key=$(sed -n 3p <<<"$peer")
peer_port=$(sed -n 4p <<<"$peer")
peer_runner=$(sed -n 5p <<<"$peer")
if [[ -z "${peer_host:-}" ]]; then
  echo "skip: no [remotes.<name>] in $machine_remotes or ~/.gongga/config.toml, so there is no peer to prove this against"
  exit 0
fi

port=$(free_port)
fake_port=$(free_port)
server_pid=""; fake_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work"/remote-*.pid
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
  [[ "$verbose" == "-v" ]] || rm -rf "$work"
}
trap cleanup EXIT

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
mkdir -p "$home"
# A machine root of this run's own, so the test never edits the operator's file and
# the server cannot pick up a peer this run did not write.
agents="$work/agents"
mkdir -p "$agents"
{
  echo "[remotes.$peer_name]"
  echo "host = \"$peer_host\""
  [[ -n "$peer_key" ]] && echo "identity = \"$peer_key\""
  [[ -n "$peer_port" ]] && echo "port = $peer_port"
  [[ -n "$peer_runner" ]] && echo "runner = \"$peer_runner\""
} > "$agents/remotes.toml"
export GONGGA_AGENTS_HOME="$agents"
# A remote in the wrong file, on purpose: startup must name it and refuse it.
{
  echo "[remotes.ghost]"
  echo "host = \"nobody@192.0.2.1\""
} > "$home/config.toml"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"

cat > "$work/remote-roster.toml" <<TOML
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
export GONGGA_PROVIDERS_CATALOG="$work/remote-roster.toml"
export FAKE_API_KEY=sk-fake
export FAKE_LLM_DUMP="$dump"

# The stand-in is restarted on the same port between turns: `FAKE_LLM_TOOL` is its
# environment, and a turn is the unit that needs a different tool.
fake_up() {
  if [[ -n "$fake_pid" ]]; then
    alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
    wait_free "$fake_port"
  fi
  rm -f "$work/remote-fake.pid"
  env FAKE_LLM_TOOL="$1" FAKE_LLM_TOOL_ARGS="$2" \
    python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
    "$work/remote-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake-remote.log" 2>&1 &
  for _ in $(seq 1 50); do [[ -s "$work/remote-fake.pid" ]] && break; sleep 0.2; done
  fake_pid=$(cat "$work/remote-fake.pid" 2>/dev/null)
  wait_tcp "$fake_port"
}

turns=0
wait_turn() {
  turns=$((turns + 1))
  local key="turn end session=$1"
  local seen
  seen=$(grep -c "$key" "$log" 2>/dev/null)
  for _ in $(seq 1 240); do
    [[ "$(grep -c "$key" "$log" 2>/dev/null)" -gt "$seen" ]] && return 0
    sleep 0.5
  done
  return 1
}
say_turn() { say "  -- turn $turns: $(grep "turn end session=$1" "$log" | tail -1)"; }

# The output of the newest tool result, for a check that reads what a tool answered.
last_tool() {
  python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='probe'").fetchone()[0]
row = c.execute(
    "select display from messages where session_id=? and kind='tool_result' \
     order by seq desc limit 1", (sid,)).fetchone()
print(json.loads(row[0]).get("output", "") if row else "")
INNER
}

fake_up remote_bash "{\"command\":\"mkdir -p \\\"\$HOME/$tag\\\" && printf '# $marker\\n' > \\\"\$HOME/$tag/AGENTS.md\\\" && cygpath -w \\\"\$HOME/$tag\\\" && cygpath -w \\\"\$HOME\\\"\",\"name\":\"peer\",\"task\":\"make the peer scratch\"}" || {
  echo "the stand-in never answered on 127.0.0.1:$fake_port" >&2; exit 1; }

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)
cli=("$cli_bin" --home "$home" --server "127.0.0.1:$port")

# `local` is not a remote name: naming it is refused, and the refusal says `none`.
refused=$("${cli[@]}" set_session --id probe --model fake/flash-fake --remote local 2>&1)
[[ "$refused" == *"no remote called \`local\`"* && "$refused" == *"none"* ]]
ok "naming \`local\` is refused, and the answer names \`none\`" $?

created=$("${cli[@]}" set_session --id probe --model fake/flash-fake --remote "$peer_name" 2>&1)
[[ "$created" == *'"ok":true'* ]]
ok "a session is created on $peer_name" $?

# The boundary: a remote left in the home's `config.toml` is refused, not honoured --
# it is the merged file, and a host name in it would be published by a pin move.
probe=$("${cli[@]}" set_session --id probe --model fake/flash-fake --remote ghost 2>&1)
# The answer is compact JSON, so the name's own quotes arrive escaped.
[[ "$probe" == *'no remote named \"ghost\"'* ]]
ok "a remote named only in config.toml does not exist" $?
grep -q 'these remotes are in config.toml' "$log"
ok "and startup says so, with the migrator that moves them" $?

"${cli[@]}" send_message --message "@skill(self-control) make the scratch" --session probe >/dev/null 2>&1
wait_turn probe; say_turn probe

# The peer's own path, in the spelling its own shell gave, and the file it wrote.
printed_paths=$(python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='probe'").fetchone()[0]
for (d,) in c.execute("select display from messages where session_id=? and kind='tool_result' order by seq", (sid,)):
    for line in json.loads(d).get("output", "").splitlines():
        line = line.strip()
        if len(line) > 3 and line[1] == ":" and line[2] in "\\/":
            print(line)
INNER
)
peer_scratch=$(sed -n 1p <<<"$printed_paths")
peer_home=$(sed -n 2p <<<"$printed_paths")
[[ -n "$peer_scratch" && -n "$peer_home" ]]
ok "remote_bash made the peer's scratch and named it in Windows spelling ($peer_scratch)" $?

# The session moves to that directory: what its references are read against.
moved=$("${cli[@]}" set_session --id probe --cwd "$peer_scratch" 2>&1)
[[ "$moved" == *'"ok":true'* ]]
ok "the session moves to $peer_scratch" $?

# The file tools reach the peer: write, read, replace, a second machine's refusal,
# and the same three verbs on this machine.
fake_up write_file "$(python - "$peer_scratch" "$peer_name" <<'PY'
import json, sys
print(json.dumps({"path": sys.argv[1] + "/from-tool.txt",
                  "content": "hello from the tool",
                  "remote": sys.argv[2]}))
PY
)"
"${cli[@]}" send_message --message "write the file" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"wrote"* ]]
ok "write_file reached the peer" $?

fake_up read_file "$(python - "$peer_scratch" "$peer_name" <<'PY'
import json, sys
print(json.dumps({"path": sys.argv[1] + "/from-tool.txt", "remote": sys.argv[2]}))
PY
)"
"${cli[@]}" send_message --message "read the file" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"hello from the tool"* ]]
ok "read_file reached the peer" $?

fake_up replace "$(python - "$peer_scratch" "$peer_name" <<'PY'
import json, sys
print(json.dumps({"path": sys.argv[1] + "/from-tool.txt",
                  "old": "hello", "new": "goodbye", "remote": sys.argv[2]}))
PY
)"
"${cli[@]}" send_message --message "replace in the file" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"replaced 1 occurrence"* ]]
ok "replace reached the peer" $?

fake_up read_file "$(python - "$peer_scratch" "$peer_name" <<'PY'
import json, sys
print(json.dumps({"path": sys.argv[1] + "/from-tool.txt", "remote": sys.argv[2]}))
PY
)"
"${cli[@]}" send_message --message "read the file again" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"goodbye from the tool"* ]]
ok "the replacement is what the peer holds" $?

fake_up read_file '{"path":"/x","remote":"ghost"}'
"${cli[@]}" send_message --message "try another machine" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"ran nothing"* ]]
ok "a second machine is refused" $?

fake_up write_file "$(python - "$home" <<'PY'
import json, sys
print(json.dumps({"path": sys.argv[1] + "/from-tool-local.txt",
                  "content": "local content"}))
PY
)"
"${cli[@]}" send_message --message "write a local file" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"wrote"* ]]
ok "write_file still writes this machine" $?

fake_up read_file "$(python - "$home" <<'PY'
import json, sys
print(json.dumps({"path": sys.argv[1] + "/from-tool-local.txt"}))
PY
)"
"${cli[@]}" send_message --message "read the local file" --session probe >/dev/null 2>&1
wait_turn probe
[[ "$(last_tool)" == *"local content"* ]]
ok "read_file still reads this machine" $?

fake_up "" "{}"
"${cli[@]}" send_message --message "now say which machine you are on" --session probe >/dev/null 2>&1
wait_turn probe; say_turn probe

# A session placed where the rules file already is: its first prompt carries it.
created2=$("${cli[@]}" set_session --id probe2 --model fake/flash-fake --remote "$peer_name" --cwd "$peer_scratch" 2>&1)
[[ "$created2" == *'"ok":true'* ]]
ok "a second session is placed on the peer" $?
"${cli[@]}" send_message --message "hello" --session probe2 >/dev/null 2>&1
wait_turn probe2
python - "$home" "$marker" <<'INNER'
import json, sqlite3, sys
home, marker = sys.argv[1], sys.argv[2]
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='probe2'").fetchone()[0]
last = [json.loads(d) for (d,) in c.execute(
    "select display from messages where session_id=? and kind='system' order by seq desc limit 1", (sid,))][0]
assert "@file(AGENTS.md)" in last["rooted"], last["rooted"]
assert marker in (last["index"] or ""), (last["index"] or "")[:300]
INNER
ok "a placed session's first prompt carries the peer's own AGENTS.md" $?

fake_up bash "{\"command\":\"pwd\",\"name\":\"here\",\"task\":\"prove this machine\"}"
"${cli[@]}" send_message --message "and a shell here" --session probe >/dev/null 2>&1
wait_turn probe; say_turn probe

fake_up list_bash "{}"
"${cli[@]}" send_message --message "which shells do you have" --session probe >/dev/null 2>&1
wait_turn probe; say_turn probe

python - "$home" "$peer_scratch" <<'INNER'
import json, sqlite3, sys
home, peer_scratch = sys.argv[1], sys.argv[2]
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='probe'").fetchone()[0]
results = [json.loads(d).get("output", "") for (d,) in c.execute(
    "select display from messages where session_id=? and kind='tool_result' order by seq", (sid,))]


def payload(text):
    """The JSON a listing answered with; a result also ends with its elapsed time."""
    start = text.find("{")
    if start < 0:
        return None
    value, _ = json.JSONDecoder().raw_decode(text[start:])
    return value if isinstance(value, dict) and "bash" in value else None


machines = {}
for text in results:
    for row in (payload(text) or {}).get("bash") or []:
        machines[row["name"]] = row["machine"]
# `here` is the shell `bash` made on this machine; `default` is the one that exists
# without being asked for, so it is the directory-owning machine's -- the remote's.
assert machines.get("here") == "local", machines
assert machines.get("default") == "remote", machines
# `bash` ran here, so its `pwd` is not the peer's directory.
printed = [r.strip().splitlines()[0].strip() for r in results if r.strip()]
here = [p for p in printed if p.startswith("/") and "/c/" in p.lower()]
assert here, printed
assert not any(p.lower().startswith(peer_scratch.lower().replace("\\", "/")) for p in here), here
INNER
ok "one name space, each name reporting the machine that holds it" $?

python - "$dump" <<'INNER'
import json, sys
seen = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
names = {t["name"] for t in seen[0].get("tools") or []}
assert {"bash", "remote_bash"} <= names, names
INNER
ok "a session with a remote is offered both tools" $?

# Out of the scratch before it goes: a move ends every shell, so nothing on the peer
# is standing in the directory the next command removes.
back=$("${cli[@]}" set_session --id probe --cwd "$peer_home" 2>&1)
[[ "$back" == *'"ok":true'* ]]
ok "the session moves out of its scratch" $?

fake_up remote_bash "{\"command\":\"rm -rf \\\"\$HOME/$tag\\\" && echo peer-clean\",\"name\":\"cleanup\",\"task\":\"remove the peer scratch\"}"
"${cli[@]}" send_message --message "clean up the peer" --session probe >/dev/null 2>&1
wait_turn probe; say_turn probe
python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='probe'").fetchone()[0]
last = [json.loads(d).get("output", "") for (d,) in c.execute(
    "select display from messages where session_id=? and kind='tool_result' order by seq desc limit 1", (sid,))][0]
assert "peer-clean" in last, last
INNER
ok "the peer's scratch is removed" $?

# A remote session comes home by naming `none`: no shells remain on the peer at all.
# The sticky `cwd` is this machine's argument, not the peer's, so a come-home move
# must not inherit it.
"${cli[@]}" del_sticky cwd >/dev/null 2>&1
back=$("${cli[@]}" set_session --id probe --remote none 2>&1)
[[ "$back" == *'"ok":true'* ]]
ok "a session comes home by naming \`none\`" $?
python - "$home" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
assert c.execute("select remote from sessions where name='probe'").fetchone()[0] is None
INNER
ok "and the row no longer names a machine" $?

elapsed=$(( $(date +%s) - started ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s\n' "$elapsed" "$sha"
else
  printf 'FAIL · %s check(s) red · %ss · %s\n' "$fails" "$elapsed" "$sha"
fi
[[ $fails == 0 ]]
