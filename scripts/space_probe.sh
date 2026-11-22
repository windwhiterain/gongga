#!/usr/bin/env bash
# A peer's references are read through the session's one Space, on the peer, and one
# dial serves every read of the message. Real processes: a server, a stand-in model,
# and the operator's own peer.
#
# One line of verdict at the end.
set -uo pipefail
started=$(date +%s)
root=/c/resource/gongga/.worktrees/space-unified
bin="$root/target/debug"
work="$root/.dev/spaceprobe"
home="$work/home"
tag="space-probe-$$"
log="$work/server.log"
dump="$work/requests.jsonl"
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

peer=/c/Users/17124/.agents/remotes.toml
peer_name=$(grep -oP '^\[remotes\.\K[^\]]+' "$peer" | head -1)
[[ -n "$peer_name" ]] || { echo "skip: no remote configured"; exit 0; }

cleanup() {
  local pid; pid=$(tr -d '\r\n' < "$home/server.lock" 2>/dev/null)
  [[ -n "${pid:-}" ]] && tasklist //FI "PID eq $pid" //NH 2>/dev/null | grep -q "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  [[ -n "${fake_pid:-}" ]] && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  sleep 1
}
trap cleanup EXIT

rm -rf "$work"; mkdir -p "$work"
# The operator's own machine root and roster, so the peer is the one they configured.
export GONGGA_AGENTS_HOME="$work/agents"
mkdir -p "$work/agents"
cp /c/Users/17124/.agents/remotes.toml "$work/agents/remotes.toml"
# The stand-in model's port, before the roster that names it.
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
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/roster.toml"
export FAKE_API_KEY=sk-fake
export FAKE_LLM_DUMP="$dump"

# The pid is the one the program wrote down: `$!` here is a shell number, not the
# Windows pid, and a test that kills the wrong one leaves a stand-in running for ever.
env python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake.pid" 2>/dev/null)
for _ in $(seq 1 50); do python -c "import socket,sys
s=socket.socket();sys.exit(0 if s.connect_ex(('127.0.0.1',$fake_port))==0 else 1)" && break; sleep 0.2; done

port=$(python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
for _ in $(seq 1 60); do [[ -x "$home/bin/gongga-cli" ]] && break; sleep 0.2; done
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# A session on the peer, and a directory there with two files a message can name.
created=$("${cli[@]}" '{"name":"set_session","args":{"id":"p","model":"fake/flash-fake","cwd":"remote://'"$peer_name"'//"}}' 2>&1)
echo "  set_session said: ${created:0:300}"
[[ "$created" == *'"ok":true'* ]]; ok "a session is created on $peer_name" $?

"${cli[@]}" '{"name":"send_message","args":{"message":"make the scratch","session":"p"}}' >/dev/null 2>&1
for _ in $(seq 1 240); do grep -q 'turn end session=p' "$log" 2>/dev/null && break; sleep 0.5; done

raw=$("${cli[@]}" '{"name":"exec","args":{"session":"p","bin":"bash","id":"bash/one","args":["-lc","mkdir -p \"$HOME/'"$tag"'\" && printf \"peer-alpha\\n\" > \"$HOME/'"$tag"'/a.md\" && printf \"peer-beta\\n\" > \"$HOME/'"$tag"'/b.md\" && cygpath -w \"$HOME/'"$tag"'\""],"wait":30000}}' 2>&1)
printf '%s\n' "$raw" > "$work/exec-answer.json"
scratch=$(python -c 'import json,sys
for line in open(sys.argv[1], encoding="utf-8"):
    line=line.strip()
    if not line: continue
    try: v=json.loads(line)
    except Exception: continue
    ev=v.get("v",{}).get("event",{})
    if ev.get("t")=="cmd_result":
        inner=json.loads(ev.get("message","{}"))
        out=(inner.get("output") or "").strip()
        if out: print(out.splitlines()[0]); break' "$work/exec-answer.json" | tr -d '\r')
ok "exec made a peer directory and named it ($scratch)" $([[ -n "$scratch" ]] && echo 0 || echo 1)

move_json=$(python -c 'import json,sys
print(json.dumps({"name":"set_session","args":{"id":"p","cwd":sys.argv[1]}}))' "$scratch")
moved=$("${cli[@]}" "$move_json" 2>&1)
[[ "$moved" == *'"ok":true'* ]] || echo "  set_session said: ${moved:0:300}"
[[ "$moved" == *'"ok":true'* ]]; ok "the session moves there" $?

before=$(wc -l < "$log" 2>/dev/null || echo 0)
"${cli[@]}" '{"name":"send_message","args":{"message":"read @file(a.md) and @file(b.md) and @file(./a.md)","session":"p"}}' >/dev/null 2>&1
for _ in $(seq 1 240); do
  [[ "$(grep -c 'turn end session=p' "$log" 2>/dev/null)" -ge 2 ]] && break; sleep 0.5
done

# The request the model was sent: both peer files, read on the peer.
python - "$dump" > "$work/check.txt" 2>&1 <<'INNER'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8") if l.strip()]
blob = json.dumps(rows)
print("alpha", "peer-alpha" in blob)
print("beta", "peer-beta" in blob)
INNER
grep -q 'alpha True' "$work/check.txt" && grep -q 'beta True' "$work/check.txt"
ok "the peer's two files reached the request ($(tr '\n' ' ' < "$work/check.txt"))" $?

# One connection served them: the read path dialled the peer's runner once for the
# whole message, not once per file. Counted from the server's own log.
dials=$(grep -c 'placing this build.s pair instead' "$log" 2>/dev/null)
dials=${dials:-0}
dials=$(printf '%s' "$dials" | tr -dc '0-9')
[[ -n "$dials" ]] || dials=0
ok "the peer's runner was placed $dials time(s) for the whole message" $([[ "$dials" -ge 1 && "$dials" -le 2 ]] && echo 0 || echo 1)

"${cli[@]}" '{"name":"exec","args":{"session":"p","bin":"bash","id":"bash/clean","args":["-lc","rm -rf \"$HOME/'"$tag"'\""],"wait":20000}}' >/dev/null 2>&1
elapsed=$(($(date +%s) - ${started:-$(date +%s)}))
if [[ $fails == 0 ]]; then printf 'PASS · %ss · %s\n' "$elapsed" "$(git -C "$root" rev-parse --short HEAD)"
else printf 'FAIL · %s check(s) red · %ss\n' "$fails" "$elapsed"; fi
[[ $fails == 0 ]]