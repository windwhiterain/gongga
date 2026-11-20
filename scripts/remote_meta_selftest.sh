#!/usr/bin/env bash
# A reference names its machine, and a peer's own checkout skills are skills here.
# Real processes: a server, a stand-in model, and the operator's own peer.
#
# One line of verdict at the end.
set -uo pipefail
started=$(date +%s)
# The checkout this script lives in, whatever it is called: a worktree moves, and a
# build under it is the one these checks speak to.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/remotemeta"
home="$work/home"
tag="remote-meta-$$"
log="$work/server.log"
dump="$work/requests.jsonl"
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

peer_name=$(grep -oP '^\[remotes\.\K[^\]]+' /c/Users/17124/.agents/remotes.toml | head -1)
[[ -n "$peer_name" ]] || { echo "skip: no remote configured"; exit 0; }

# The child serves and holds the lock; its watchdog is the process that started it, and
# nothing names that one in a file. Both are stopped, by pid, read from the OS.
# See `AGENTS.md`, "绝不按镜像名杀进程".
cleanup() {
  local pid supervisor
  pid=$(cat "$home/server.lock" 2>/dev/null | tr -d '\r\n')
  if [[ "$pid" =~ ^[0-9]+$ ]]; then
    supervisor=$(powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=$pid').ParentProcessId" 2>/dev/null | tr -d '\r\n ')
    taskkill //PID "$pid" //T //F >/dev/null 2>&1
    # The watchdog respawns what it waits on; stop it too, and only if it is one.
    if [[ -n "${supervisor:-}" ]] && powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=$supervisor').CommandLine" 2>/dev/null | grep -q -F -e "$home" -e "$(cygpath -m "$home" 2>/dev/null || echo "$home")"; then
      taskkill //PID "$supervisor" //T //F >/dev/null 2>&1
    fi
  fi
  [[ -n "${fake_pid:-}" ]] && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  sleep 1
}
trap cleanup EXIT

# Whoever is serving this home from an earlier run holds its lock: stopped by the home
# its own command line names, never by an image name. See `AGENTS.md`.
native_home=$(cygpath -m "$home" 2>/dev/null || echo "$home")
left=$(powershell -NoProfile -Command "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$home*' -or \$_.CommandLine -like '*--home *$native_home*' } | ForEach-Object { \$_.ProcessId }" 2>/dev/null | tr -d '\r' | tr '\n' ' ')
for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
[[ -n "${left// /}" ]] && sleep 2

rm -rf "$work" 2>/dev/null; mkdir -p "$work/agents" "$home"
export GONGGA_AGENTS_HOME="$work/agents"
cp /c/Users/17124/.agents/remotes.toml "$work/agents/remotes.toml"
# The content this machine already has: cloning it from GitHub is not something a test
# may depend on, and a home without one does not start.
cp -r /c/Users/17124/.gongga/ship "$home/ship"
# The image skill is what this branch is proving: its own copy is the one under test.
cp /c/resource/gongga/ship/skills/image/__init__.py "$home/ship/skills/image/__init__.py"
cp /c/resource/gongga/ship/skills/image/SKILL.md "$home/ship/skills/image/SKILL.md"

fake_port=$(python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
cat > "$work/roster.toml" <<TOML
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
  modalities = ["text", "image"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/roster.toml"
export FAKE_API_KEY=sk-fake
export FAKE_LLM_DUMP="$dump"

# The pid is the one the program wrote down: `$!` here is a shell number, not the
# Windows pid, and a test that kills the wrong one leaves a stand-in running for ever.
# See `AGENTS.md`, "绝不按镜像名杀进程".
env python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake.pid" 2>/dev/null)
for _ in $(seq 1 50); do python -c "import socket,sys
s=socket.socket();sys.exit(0 if s.connect_ex(('127.0.0.1',$fake_port))==0 else 1)" && break; sleep 0.2; done

port=$(python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION \
  -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 400); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
# This run's server must be this run's: a home whose lock names another process is a
# home somebody else is serving, and a command sent there would be theirs to answer.
# The path is compared in both spellings -- this shell says `/c/...`, a Windows process
# says `C:/...` -- or every lock looks like somebody else's.
lock=$(cat "$home/server.lock" 2>/dev/null | tr -d '\r\n')
native=$(cygpath -m "$home" 2>/dev/null || echo "$home")
if [[ ! "$lock" =~ ^[0-9]+$ ]] || ! powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=$lock').CommandLine" 2>/dev/null | grep -q -F -e "$home" -e "$native"; then
  echo "refusing to run: the lock in $home does not name a server of this home (got '$lock')" >&2
  exit 1
fi
for _ in $(seq 1 60); do [[ -x "$home/bin/gongga-cli" ]] && break; sleep 0.2; done
# The CLI is the env-less one: `gongga_cli.py --home --server` beats an inherited
# GONGGA_HOME, and this shell's may name somebody else's home.
cli=(python "$home/ship/python/gongga_cli.py" --home "$home" --server "127.0.0.1:$port")

created=$("${cli[@]}" '{"name":"set_session","args":{"id":"p","model":"fake/flash-fake","remote":"'"$peer_name"'"}}' 2>&1)
[[ "$created" == *'"ok":true'* ]]; ok "a session is created on $peer_name" $?

"${cli[@]}" '{"name":"send_message","args":{"message":"make the scratch","session":"p"}}' >/dev/null 2>&1
for _ in $(seq 1 240); do grep -q 'turn end session=p' "$log" 2>/dev/null && break; sleep 0.5; done

# One scratch on the peer: a picture, a text file, and a project skill of its own. The
# picture is a real PNG: the server reads its header chunks, so a made-up signature is
# refused and would prove nothing about a reference.
png=$(python -c 'import base64
print(base64.b64encode(bytes.fromhex("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082")).decode())')
# What the peer side makes: a picture, a text file, and a project skill of its own. Built
# here so the request below is one `json.dumps`, not a quoted nest.
mk=$(python - "$tag" "$png" <<'INNER'
import sys
tag, png = sys.argv[1], sys.argv[2]
home = "$HOME/" + tag
print(
    f'mkdir -p "{home}/.agents/skills/peer-only" && '
    f'printf "# peer rules\\n" > "{home}/notes.md" && '
    f'printf "peer-skill-body\\n" > "{home}/.agents/skills/peer-only/SKILL.md" && '
    f'printf %s "{png}" | base64 -d > "{home}/shot.png" && '
    f'cygpath -w "{home}"'
)
INNER
)
scratch_request() {
  python -c 'import json,sys
print(json.dumps({"name":"exec","args":{"session":"p","bin":"bash","id":"bash/one","args":["-lc",sys.argv[1]],"wait":30000}}))' "$mk"
}
raw=$("${cli[@]}" "$(scratch_request)" 2>&1)
printf '%s\n' "$raw" > "$work/exec.json"
read_scratch() {
  python -c 'import json,sys
for line in open(sys.argv[1], encoding="utf-8"):
    line=line.strip()
    if not line: continue
    try: v=json.loads(line)
    except Exception: continue
    ev=v.get("v",{}).get("event",{})
    if ev.get("t")=="cmd_result":
        inner=json.loads(ev.get("message","{}"))
        out=(inner.get("output") or "").strip()
        if out: print(out.splitlines()[0]); break' "$work/exec.json" | tr -d '\r'
}
scratch=$(read_scratch)
# The first call answers what the channel said so far. A start that never made one is
# asked for again, whole: a read by name answers "no channel" and leaves the run without
# a peer, which then reads as four red checks about the peer's files.
if [[ -z "$scratch" ]]; then
  sleep 2
  "${cli[@]}" "$(scratch_request)" > "$work/exec.json" 2>&1
  scratch=$(read_scratch)
fi
ok "the peer's scratch is made ($scratch)" $([[ -n "$scratch" ]] && echo 0 || echo 1)

move=$(python -c 'import json,sys
print(json.dumps({"name":"set_session","args":{"id":"p","cwd":sys.argv[1]}}))' "$scratch")
moved=$("${cli[@]}" "$move" 2>&1)
[[ "$moved" == *'"ok":true'* ]]; ok "the session stands on the peer's scratch" $?

before=$(wc -l < "$dump" 2>/dev/null || echo 0)
"${cli[@]}" '{"name":"send_message","args":{"message":"read @file(notes.md) and @skill(peer-only)","session":"p"}}' >/dev/null 2>&1
for _ in $(seq 1 240); do
  [[ "$(grep -c 'turn end session=p' "$log" 2>/dev/null)" -ge 2 ]] && break; sleep 0.5
done

# 1: a peer file, named by a message whose session already stands there.
python - "$dump" > "$work/a.txt" 2>&1 <<'INNER'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8") if l.strip()]
blob = json.dumps(rows[-1] if rows else {})
print("peer-rules-in-request", "peer rules" in blob)
print("nothing-sent", "" not in blob)
INNER
grep -q 'peer-rules-in-request True' "$work/a.txt"
verdict=$?
ok "the peer's @file reached the request ($(tr '\n' ' ' < "$work/a.txt"))" "$verdict"

# 2: a skill in the peer's own checkout is a skill here.
python - "$home" > "$work/b.txt" 2>&1 <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("select display from messages where kind='tool_result' order by seq desc limit 1").fetchone()
print(row[0][:200] if row else "none")
INNER
lsout=$("${cli[@]}" '{"name":"ls_skill","args":{"session":"p","path":"peer-only"}}' 2>&1)
[[ "$lsout" == *"peer-only"* ]]; ok "ls_skill finds the peer's own project skill" $?

# 3: the reference names a machine out loud: @file(remote://<peer>/x).
outts=$("${cli[@]}" '{"name":"send_message","args":{"message":"read @file(remote://'"$peer_name"'/notes.md) from the peer","session":"p"}}' 2>&1)
for _ in $(seq 1 240); do
  [[ "$(grep -c 'turn end session=p' "$log" 2>/dev/null)" -ge 3 ]] && break; sleep 0.5
done
python - "$dump" > "$work/c.txt" 2>&1 <<'INNER'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8") if l.strip()]
blob = json.dumps(rows[-1] if rows else {})
print("named-remote-read", "peer rules" in blob)
INNER
grep -q 'named-remote-read True' "$work/c.txt"
verdict=$?
ok "an explicit remote:// reference is read there ($(cat "$work/c.txt"))" "$verdict"

# 4: the image skill prints a peer reference.
img=$("${cli[@]}" '{"name":"run_skill","args":{"session":"p","id":"image","cmd":{"name":"show","args":{"paths":["shot.png"]}}}}' 2>&1)
printf '%s\n' "$img" > "$work/img.json"
[[ "$img" == *"@image(remote://$peer_name/"* ]]; ok "the image skill prints a remote reference" $?

# 4b: a path this machine holds is named as this machine's, whatever session it is asked
# in. Naming the peer for a clipboard's own file is a reference nothing can read there.
printf %s "$png" | base64 -d > "$work/skill-local.png"
localmsg=$(python - "$work/skill-local.png" <<'INNER'
import json, os, sys
print(json.dumps({"name": "run_skill", "args": {
    "session": "p", "id": "image",
    "cmd": {"name": "show", "args": {"paths": [os.path.abspath(sys.argv[1])]}}}}))
INNER
)
localref=$("${cli[@]}" "$localmsg" 2>&1)
printf '%s\n' "$localref" > "$work/skill-local.json"
[[ "$localref" == *"@image(remote://local/"* ]]; ok "the image skill names this machine for the path it holds" $?

# 5: that reference, written into a message, is a picture the request carries: the
# bytes came off the peer, and no local file of that name is involved.
imgmsg=$(python -c 'import json,sys
print(json.dumps({"name":"send_message","args":{"session":"p","message":"look at @image(remote://"+sys.argv[1]+"/"+sys.argv[2]+"/shot.png)"}}))' "$peer_name" "$scratch")
imgsend=$("${cli[@]}" "$imgmsg" 2>&1)
printf '%s\n' "$imgsend" > "$work/imgsend.txt"
for _ in $(seq 1 240); do
  [[ "$(grep -c 'turn end session=p' "$log" 2>/dev/null)" -ge 4 ]] && break; sleep 0.5
done
python - "$dump" "$home" > "$work/d.txt" 2>&1 <<'INNER'
import json, sqlite3, sys
dump, home = sys.argv[1], sys.argv[2]
rows = [json.loads(l) for l in open(dump, encoding="utf-8") if l.strip()]
blob = json.dumps(rows[-1] if rows else {})
print("image-part-in-request", '"input_image"' in blob or '"image_url"' in blob)
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
kept = None
for (d,) in c.execute("select display from messages order by seq desc limit 12"):
    row = json.loads(d)
    if row.get("role") == "user" and row.get("images"):
        kept = row
        break
print("picture-copied", bool(kept and kept.get("images")))
print("picture-path", (kept or {}).get("images", [{}])[0].get("path", ""))
INNER
both() { local a=1 b=1; grep -q "$1" "$3" && a=0; grep -q "$2" "$3" && b=0; [[ $a == 0 && $b == 0 ]]; }
both 'image-part-in-request True' 'picture-copied True' "$work/d.txt"
verdict=$?
ok "a peer picture is attached to the request ($(tr '\n' ' ' < "$work/d.txt"))" "$verdict"

# 6: the clipboard's own picture, in a session working on a peer. The TUI writes it as
# `@image(remote://local/<path>)` -- the machine stated -- and the server must read it
# here: a bare path would be read on the peer, where a clipboard file does not exist.
printf %s "$png" | base64 -d > "$work/local-paste.png"
pastemsg=$(python - "$work/local-paste.png" <<'INNER'
import json, os, sys
path = os.path.abspath(sys.argv[1])
print(json.dumps({"name": "send_message", "args": {
    "session": "p", "message": "look at @image(remote://local/" + path + ")"}}))
INNER
)
printf '%s\n' "$pastemsg" > "$work/paste-cmd.txt"
"${cli[@]}" "$pastemsg" > "$work/paste-send.txt" 2>&1
for _ in $(seq 1 240); do
  [[ "$(grep -c 'turn end session=p' "$log" 2>/dev/null)" -ge 5 ]] && break; sleep 0.5
done
python - "$dump" "$home" > "$work/e.txt" 2>&1 <<'INNER'
import json, sqlite3, sys
dump, home = sys.argv[1], sys.argv[2]
rows = [json.loads(l) for l in open(dump, encoding="utf-8") if l.strip()]
blob = json.dumps(rows[-1] if rows else {})
print("image-part-in-request", '"input_image"' in blob or '"image_url"' in blob)
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
kept = None
for (d,) in c.execute("select display from messages order by seq desc limit 12"):
    row = json.loads(d)
    if row.get("role") == "user" and row.get("images"):
        kept = row
        break
print("picture-copied", bool(kept and kept.get("images")))
print("picture-path", (kept or {}).get("images", [{}])[0].get("path", ""))
INNER
both 'image-part-in-request True' 'picture-copied True' "$work/e.txt"
verdict=$?
ok "a picture this machine holds reaches the request ($(tr '\n' ' ' < "$work/e.txt"))" "$verdict"
"${cli[@]}" '{"name":"exec","args":{"session":"p","bin":"bash","id":"bash/clean","args":["-lc","rm -rf \"$HOME/'"$tag"'\""],"wait":20000}}' >/dev/null 2>&1
elapsed=$(($(date +%s) - started))
if [[ $fails == 0 ]]; then printf 'PASS · %ss · %s\n' "$elapsed" "$(git -C "$root" rev-parse --short HEAD)"
else printf 'FAIL · %s check(s) red · %ss\n' "$fails" "$elapsed"; fi
[[ $fails == 0 ]]