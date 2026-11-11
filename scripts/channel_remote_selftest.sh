#!/usr/bin/env bash
# A channel on a peer: a program the peer's own relay keeps, whose files this server
# reads through one connection. Nothing here can be seen without both machines -- the
# directory is chosen on the peer, the relay must leave this server's job, and a
# restart must find the channel still answering -- so this runs a real server against
# the peer named in the operator's own remotes file.
#
# One line of verdict at the end; `-v` keeps the scratch and prints every answer.
# Skipped, not failed, when this machine configures no peer: there would be nothing to
# prove, and the peer's runner must speak this build's protocol.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and a Windows program -- the
# server is one -- reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/channel-remote-home"
log="$work/channel-remote.log"
verbose=${1:-}
began=$(date +%s)
tag="gongga-channel-remote-$$"

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
has() { [[ "$1" == *"$2"* ]]; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
server_bin="$bin/gongga-server"
[[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"

# The peer this machine configures, in either of the two spellings the file has had.
peer_source="${GONGGA_AGENTS_HOME:-$HOME/.agents}/remotes.toml"
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
  echo "skip: no [remotes.<name>] in $peer_source, so there is no peer to place a channel on"
  exit 0
fi

free_port() {
  python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}
# A count, not a grep: this run starts its server twice, and the first line is not
# evidence about the second start.
wait_listening() {
  local seen
  seen=$(grep -c "listening (UDP) local=127.0.0.1:$port" "$log" 2>/dev/null)
  seen=${seen:-0}
  for _ in $(seq 1 300); do
    [[ "$(grep -c "listening (UDP) local=127.0.0.1:$port" "$log" 2>/dev/null)" -gt "$seen" ]] && return 0
    sleep 0.2
  done
  return 1
}
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
cleanup() {
  for pid in $(serving_home) "$server_pid" "$fake_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  # The peer's own directory: removed through the peer's shell, by the name it gave.
  if [[ -n "${peer_scratch:-}" ]]; then
    ssh -o BatchMode=yes -o ConnectTimeout=8 ${peer_key:+-i "$peer_key"} ${peer_port:+-p "$peer_port"} \
      "$peer_host" -- "bash -lc 'rm -rf \"$peer_scratch\"'" </dev/null >/dev/null 2>&1
  fi
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
  [[ "$verbose" == "-v" ]] || rm -rf "$work"
}
server_pid=""
peer_scratch=""
trap cleanup EXIT

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
mkdir -p "$work" "$home"
# This checkout's own content: a home that has none reaches for the network, and a
# channel test must not depend on that answer. It is cloned rather than copied -- a
# submodule's `.git` is a pointer into the superproject, which a copy cannot open.
git clone --quiet "$root/ship" "$home/ship" 2>/dev/null || cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"

# Which distribution a peer runs is the server's answer, not this file's: no runner is
# named below, so a run that reaches a program proves the peer chose one that speaks
# this build -- or that this build placed its own there.

# A scratch directory on the peer, made and named by the peer's own shell.
peer_scratch=$(ssh -o BatchMode=yes -o ConnectTimeout=8 ${peer_key:+-i "$peer_key"} ${peer_port:+-p "$peer_port"} \
  "$peer_host" -- "bash -lc 'mkdir -p \"\$HOME/$tag\" && cygpath -w \"\$HOME/$tag\"'" </dev/null 2>/dev/null | tr -d '\r' | tail -1)
[[ -n "$peer_scratch" ]] || { echo "the peer made no scratch directory" >&2; exit 1; }
say "peer scratch: $peer_scratch"

# This run's own remotes file, so the operator's is never read twice or written.
export GONGGA_REMOTES_CATALOG="$work/channel-remote-remotes.toml"
# No `runner` line on purpose: a peer is asked for the distribution that speaks this
# build's protocol, and one is placed there when it has none. Naming a path is what
# made two ends disagree, so the test never names one.
{
  echo "[remotes.$peer_name]"
  echo "host = \"$peer_host\""
  [[ -n "$peer_key" ]] && echo "identity = \"$peer_key\""
  [[ -n "$peer_port" ]] && echo "port = $peer_port"
} > "$GONGGA_REMOTES_CATALOG"

export GONGGA_HOME="$home"
port=$(free_port)
fake_port=$(free_port)
fake_pid=""
# A session's first prompt starts a turn. A real model would then open channels of
# its own in this home while the test asks about its own, so a stand-in answers.
cat > "$work/channel-remote-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "CHANNEL_REMOTE_SELFTEST_KEY"

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
export GONGGA_PROVIDERS_CATALOG="$work/channel-remote-roster.toml"
export CHANNEL_REMOTE_SELFTEST_KEY=sk-fake
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/channel-remote-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/channel-remote-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/channel-remote-fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/channel-remote-fake.pid" 2>/dev/null)
for _ in $(seq 1 100); do
  python -c 'import socket,sys
s=socket.socket();s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$fake_port" && break
  sleep 0.2
done
start_server() {
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
      -u GONGGA_API_KEY_ENV -u GONGGA_SESSION \
      "$server_bin" --home "$home" --bind "127.0.0.1:$port" \
      --log info >>"$log" 2>&1 &
  for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
  server_pid=$(child_pid)
  wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
}
start_server
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# One field of the answer, written with no trailing newline: Python on Windows ends a
# `print` with `\r\n`, and `$( )` strips only the `\n`.
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

# 1. A one-shot program on the peer, printing the directory it ran in: the peer's
#    scratch, not a directory this machine has of the same name.
once=$("${cli[@]}" '{"name":"exec","args":{"session":"main","bin":"bash","id":"'"${peer_name}-once"'","remote":"'"$peer_name"'","cwd":"'"$peer_scratch"'","args":["-lc","pwd"],"wait":30000}}' 2>&1)
say "$once"
ran=$(field "$once" output | tr -d '\r\n')
say "ran in: $ran  exit: $(field "$once" exit)"
ok "exec on $peer_name runs the program there" "$([[ -n "$ran" && "$(field "$once" exit)" == 0 ]] && echo 0 || echo 1)"

# 2. One channel, two commands: the peer's relay keeps the program between calls, and
#    the cursor is this server's, so each call answers only what it said.
# `--wait 0`: a shell that has been opened has said nothing yet, and waiting for a
# window that cannot arrive is what the caller's own clock is for.
shell=$("${cli[@]}" '{"name":"exec","args":{"session":"main","bin":"bash","id":"'"$peer_name-shell"'","remote":"'"$peer_name"'","cwd":"'"$peer_scratch"'","wait":0}}' 2>&1)
say "$shell"
shell_id=$(field "$shell" id)
ok "a channel on $peer_name opens" "$([[ -n "$shell_id" ]] && echo 0 || echo 1)"
one=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"echo one","wait":20000}}' 2>&1)
two=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"echo two","wait":20000}}' 2>&1)
say "one: $(field "$one" output) / two: $(field "$two" output)"
ok "the same channel answers a second command" \
  "$([[ "$(field "$one" output | tr -d '\r\n')" == one && "$(field "$two" output | tr -d '\r\n')" == two ]] && echo 0 || echo 1)"

# 3. The listing names the machine and the directory the peer chose.
listed=$("${cli[@]}" '{"name":"ls_channel","args":{"session":"main","path":""}}' 2>&1)
say "$listed"
# The answer's message is JSON inside JSON, so it is parsed, not grepped.
row=$(python - "$listed" "$shell_id" <<'INNER'
import json, sys
for line in sys.argv[1].splitlines():
    if "cmd_result" not in line:
        continue
    message = json.loads(line)["v"]["event"].get("message", "")
    try:
        rows = json.loads(message)
    except Exception:
        continue
    for row in rows if isinstance(rows, list) else []:
        if row.get("id") == sys.argv[2]:
            print(json.dumps(row))
    break
INNER
)
peer_dir=$(python -c 'import json,sys
row = json.loads(sys.argv[1]) if sys.argv[1].strip() else {}
print(row.get("dir", ""))' "$row")
peer_remote=$(python -c 'import json,sys
row = json.loads(sys.argv[1]) if sys.argv[1].strip() else {}
print(row.get("remote") or "")' "$row")
ok "ls_channel says the channel is on $peer_name" "$([[ "$peer_remote" == "$peer_name" ]] && echo 0 || echo 1)"
say "the peer placed it at: $peer_dir"
# A machine's own home on the peer, not a path this one could have made up.
ok "the directory is one the peer chose under its own home" \
  "$(has "$peer_dir" '/.gongga/channels/ch-' && echo 0 || echo 1)"

# 4. The invariant: the relay is the peer's, so a server that comes back finds the
#    program still there and still framed.
taskkill //PID "$server_pid" //T //F >/dev/null 2>&1
for _ in $(seq 1 60); do alive "$server_pid" || break; sleep 0.2; done
sleep 1
start_server
after=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","input":"echo after-restart","wait":20000}}' 2>&1)
say "$after"
ok "a channel survives the server it was started from" \
  "$([[ "$(field "$after" output | tr -d '\r\n')" == after-restart ]] && echo 0 || echo 1)"

# 5. `del_channel` ends it: the program is gone, and the row with it.
ended=$("${cli[@]}" '{"name":"del_channel","args":{"session":"main","id":"'"$shell_id"'"}}' 2>&1)
say "$ended"
ok "del_channel ends a channel on $peer_name" "$(has "$ended" 'ended channel' && echo 0 || echo 1)"
gone=$("${cli[@]}" '{"name":"exec","args":{"session":"main","id":"'"$shell_id"'","wait":0}}' 2>&1)
ok "the ended channel is no longer there to read" "$(has "$gone" 'no channel' && echo 0 || echo 1)"

took=$(( $(date +%s) - began ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s · peer %s (%s)\n' "$took" "$sha" "$peer_name" "$peer_scratch"
else
  printf 'FAIL · %ss · %s · %s check(s)\n' "$took" "$sha" "$fails"
  exit 1
fi
