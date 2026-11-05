#!/usr/bin/env bash
# Client state is the window's: a one-shot `gongga-cli` reads none of it, writes
# none of it, and does not offer the commands that carry it.
#
# Real server, real command layer, a `cli.toml` that says where a bare command
# "should" go -- and one line of verdict at the end. `-v` prints each check.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/state-home"
log="$work/state.log"
roster="$work/state-roster.toml"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-cli" >&2; exit 1; }

server_bin="$bin/gongga-server"; cli_bin="$bin/gongga-cli"
[[ -x "$server_bin" ]] || { server_bin="$bin/gongga-server.exe"; cli_bin="$bin/gongga-cli.exe"; }

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
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }
# Whose server this is: the pid in this home's lock, never a name.
serving_home() { child_pid; }

port=$(free_port)
cleanup() {
  local pid; pid=$(serving_home)
  [[ -n "${pid:-}" ]] && alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  # The ship checkout under the home is a child of the server: one more round.
  sleep 2
  rm -rf "$home" 2>/dev/null || {
    sleep 2
    rm -rf "$home" 2>/dev/null || echo "note: $home is still held by a process" >&2
  }
}
trap cleanup EXIT

mkdir -p "$work"
rm -rf "$home"; mkdir -p "$home"
: > "$log"
export GONGGA_HOME="$home"

cat > "$roster" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "STATE_SELFTEST_KEY"
cache_ttl_tiers = { "5m" = 300, "1h" = 3600 }

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:1/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Fake Flash"
  context = 100000
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$roster"
export STATE_SELFTEST_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)

# The state a window would have left behind: a session, a model, and a level.
# `s1` exists, so a command that inherited the entry would land in its transcript.
cat > "$home/cli.toml" <<'TOML'
[[sticky_cmd_arguments]]
name = "session"
value = "s1"

[[sticky_cmd_arguments]]
name = "model"
value = "fake/flash-fake"
TOML
before=$(md5sum "$home/cli.toml" | cut -d' ' -f1)

cli=("$cli_bin" --home "$home" --server "127.0.0.1:$port")
"${cli[@]}" set_session s1 --model fake/flash-fake >/dev/null 2>&1
after_create=$(md5sum "$home/cli.toml" | cut -d' ' -f1)

# 1. A required session is not filled from the window's entry: the line fails,
#    and nothing is delivered to the session the entry names.
sent=$("${cli[@]}" send_message --message "should not land" 2>&1); code=$?
[[ $code -ne 0 && "$sent" == *"missing required argument"* ]]
ok "send_message with no --name is refused instead of using the sticky session" $?
read_back=$("${cli[@]}" get_messages --session s1 2>&1)
[[ "$read_back" != *"should not land"* ]]
ok "the refused message never reached s1" $?

# 2. The same for a read: no sticky range, no sticky session.
got=$("${cli[@]}" get_messages 2>&1); code=$?
[[ $code -ne 0 && "$got" == *"missing required argument"* ]]
ok "get_messages with no --session is refused" $?

# 3. A command the server accepts writes nothing back: the file a window keeps is
#    the window's, and this process has no window.
[[ "$before" == "$after_create" ]]
ok "set_session leaves cli.toml byte for byte as the window wrote it" $?

# 4. The commands that carry that state are not published to a one-shot process.
for name in set_sticky del_sticky set_render set_render_session connect_server; do
  out=$("${cli[@]}" "$name" 2>&1); code=$?
  [[ $code -ne 0 && "$out" == *"unknown command"* ]]
  ok "$name is not a command this program has" $?
  say "$out"
done

# 5. `--cache_tier none` is a statement: the value that sends no tier at all, and
#    the only one a model that declares none can take.
clear=$("${cli[@]}" set_session plain --model fake/flash-fake --cache_tier none 2>&1); code=$?
[[ $code -eq 0 && "$clear" == *'"ok":true'* ]]
ok "set_session --cache_tier none is accepted" $?
say "$clear"
bad=$("${cli[@]}" set_session plain --model fake/flash-fake --cache_tier 9h 2>&1); code=$?
[[ $code -ne 0 && "$bad" == *"is not a tier this model declares"* && "$bad" == *"none"* ]]
ok "an undeclared tier is refused, and none is named among the values" $?
say "$bad"

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails -eq 0 ]]; then
  echo "PASS · 4s · client-state · $sha"
else
  echo "FAIL · $fails check(s) · client-state · $sha"
fi
exit $(( fails == 0 ? 0 : 1 ))
