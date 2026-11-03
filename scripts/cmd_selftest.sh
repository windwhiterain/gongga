#!/usr/bin/env bash
# The cmd surface per the latest SPEC, on real processes: positional binding,
# diagnosis, the tool commands, and a subscription that runs code in the
# session's own named interpreter.
#
# One line of verdict at the end; `-v` prints each check.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/cmd-home"
log="$work/cmd.log"
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
wait_tcp() { for _ in $(seq 1 100); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

port=$(free_port)
server_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
rm -rf "$home"; mkdir -p "$home"
: > "$log"

export GONGGA_HOME="$home"
cat > "$work/cmd-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "CMD_SELFTEST_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:1/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Fake Flash"
  tier = "flash"
  context = 100000
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/cmd-roster.toml"
export CMD_SELFTEST_KEY=sk-fake
"$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)
cli=("$cli_bin" --home "$home" --server "127.0.0.1:$port")

# The model never runs: no turn is started, so no provider is needed.

# 1. Positional set_session.
created=$("${cli[@]}" set_session main fake/flash-fake --cwd "$root" 2>&1)
[[ "$created" == *'"ok":true'* ]]
ok "set_session main (positional session)" $?
say "$created"

# 2. Sticky session fills the omitted first positional of del_session's sibling.
sent=$("${cli[@]}" send_message main "hello there" 2>&1)
[[ "$sent" == *'"ok":true'* ]]
ok "send_message main <message> (both positional)" $?

# 3. Diagnosis: an unknown flag names the command's syntax.
bad=$("${cli[@]}" send_message main hi --mesage x 2>&1)
[[ "$bad" == *"has no argument --mesage"* && "$bad" == *"<session> <message>"* ]]
ok "unknown flag diagnosed with the signature" $?
say "$bad"

# 4. Diagnosis: an extra word is refused with the signature.
extra=$("${cli[@]}" del_session main stray 2>&1)
[[ "$extra" == *"unexpected argument \"stray\""* && "$extra" == *"<session>"* ]]
ok "extra positional diagnosed" $?
say "$extra"

# 5. get_messages with a positional range reads the transcript back.
got=$("${cli[@]}" get_messages main 2>&1)
[[ "$got" == *'hello there'* ]]
ok "get_messages main (omitted range is the whole transcript)" $?

# 6. The bash tool as a command, in the session's persistent shell.
ran=$("${cli[@]}" bash main "echo from-cmd > cmd-selftest.txt" 2>&1)
[[ "$ran" == *'"ok":true'* ]]
ok "bash main <command> (tool command)" $?

# 7. The session's own shell kept the cd between the tool and the command:
#    the file the command wrote is where the session is.
listed=$("${cli[@]}" bash main "cat cmd-selftest.txt" 2>&1)
[[ "$listed" == *"from-cmd"* ]]
ok "the tool command meets the session's persistent shell" $?

# 8. python tool command.
py=$("${cli[@]}" python main "print(6 * 7)" 2>&1)
[[ "$py" == *"42"* ]]
ok "python main <code> (tool command)" $?

# 9. read_file tool command reads what bash wrote, through the session's dir.
read_back=$("${cli[@]}" read_file main cmd-selftest.txt 2>&1)
[[ "$read_back" == *"from-cmd"* ]]
ok "read_file main <path> (tool command)" $?

# 10. A subscription armed with a named interpreter; trigger_event fires it;
#     the action runs in that named shell, shared with the bash tool.
sub=$(GONGGA_SESSION=main "${cli[@]}" subscribe watcher '^cmd_ping$' --persistant true --bash plug --cmd "echo sub-ran >> sub.txt" 2>&1)
[[ "$sub" == *'"ok":true'* ]]
ok "subscribe watcher '^cmd_ping$' --bash plug --cmd ..." $?
say "$sub"

"${cli[@]}" bash main "echo warm > sub.txt" >/dev/null 2>&1
trig=$("${cli[@]}" trigger_event cmd_ping 2>&1)
[[ "$trig" == *'"ok":true'* ]]
ok "trigger_event cmd_ping" $?
sleep 2
seen=$("${cli[@]}" bash main "cat sub.txt" 2>&1)
[[ "$seen" == *"sub-ran"* ]]
ok "the subscription's action ran in the named shell" $?
say "$seen"

# 11. The outcome is a log row the events command reads.
row=$("${cli[@]}" events --kind subscription --session main 2>&1)
{ [[ "$row" == *"sub-ran"* ]] || [[ "$row" == *subscription* ]]; } && rc=0 || rc=1
ok "the outcome is a subscription log row" $rc
say "$row"

# 12. del_subscribe drops by name.
dropped=$(GONGGA_SESSION=main "${cli[@]}" del_subscribe watcher 2>&1)
[[ "$dropped" == *'"ok":true'* ]]
ok "del_subscribe watcher" $?

# 13. A missed subscription name is a refusal.
missed=$(GONGGA_SESSION=main "${cli[@]}" del_subscribe watcher 2>&1)
[[ "$missed" == *"no subscription named"* ]]
ok "del_subscribe refuses an unknown name" $?

# 14. interrupt_session and the rest of the positional family answer.
inter=$("${cli[@]}" interrupt_session main 2>&1)
[[ "$inter" == *'"ok":true'* || "$inter" == *"not running"* || "$inter" == *"no turn"* ]]
ok "interrupt_session main" $?
say "$inter"

# 15. The operation the window's gestures use crosses the wire: the protocol
#     carries it beside the commands, and the server answers it like one.
"${cli[@]}" set_session doomed --cwd "$root" >/dev/null 2>&1
probe=$(python "$root/scripts/operation_probe.py" --port "$port" --key "$(cat "$home/server.key")" delete_session --session doomed 2>&1)
[[ "$probe" == *'"ok":true'* ]]
ok "the delete_session operation is answered" $?
say "$probe"

# 16. The session it deleted is gone.
left=$("${cli[@]}" get_messages doomed 2>&1)
[[ "$left" == *"no session"* || "$left" == *'"ok":false'* ]]
ok "the session the operation deleted is gone" $?

rm -f "$root/cmd-selftest.txt" "$root/sub.txt"
echo
if [[ "$fails" == 0 ]]; then
  printf 'PASS · cmd selftest · %s\n' "$(git -C "$root" rev-parse --short HEAD)"
else
  printf 'FAIL · %d check(s) · %s\n' "$fails" "$(git -C "$root" rev-parse --short HEAD)"
  exit 1
fi
