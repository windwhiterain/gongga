#!/usr/bin/env bash
# SPEC's Meta references, on real processes: a name is read where the machine says
# it ends, not where a space says it starts.
#
# The three cases a word-based parser lost: prose glued in front of the `@`, prose
# glued after the path, and a skill's name running straight into Chinese. Plus the
# one a path holding a space needs: quotes.
#
# One line of verdict at the end; `-v` prints the rows it read.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${META_SELFTEST_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/meta-home"
log="$work/meta.log"
verbose=${1:-}
started=$(date +%s)

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build --workspace" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
# A port nobody holds, asked for over TCP: a UDP answer names a different port space.
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
# Every process serving THIS home, whatever the run remembers: the home has several
# spellings, and matching one of them finds nothing.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

port=$(free_port)
fake_port=$(free_port)
server_pid=""; fake_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/fake-meta.pid"
  sleep 1
  [[ -n "${META_SELFTEST_KEEP:-}" ]] && return 0
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
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"
export FAKE_LLM_DUMP="${FAKE_LLM_DUMP:-$work/meta-requests.jsonl}"
: > "$FAKE_LLM_DUMP"

# Two files whose own paths the value has to survive: a space, and a parentheses.
spacey="$work/meta-notes/a note.md"
nested="$work/meta-notes/a (1) b.md"
rm -rf "$work/meta-notes"; mkdir -p "$work/meta-notes"
printf 'a note about spacing\n' > "$spacey"
printf 'a note about parentheses\n' > "$nested"

python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/fake-meta.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake-meta.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/fake-meta.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake-meta.pid" 2>/dev/null)
if ! wait_tcp "$fake_port"; then
  echo "the fake endpoint never answered on 127.0.0.1:$fake_port" >&2
  sed -n '1,12p' "$work/fake-meta.log" >&2
  exit 1
fi

# One model, the fake, so nothing here depends on the machine's roster.
cat > "$work/meta-roster.toml" <<TOML
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
export GONGGA_PROVIDERS_CATALOG="$work/meta-roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)
# The client is the home's shim, written by the server it talks to (`docs/notes/cli-bin-gongga.md`).
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

created=$("${cli[@]}" set_session --id reader --model fake/flash-fake --cwd "$root" 2>&1)
[[ "$created" == *'"ok":true'* ]]
ok "a session is created in the repository" $?

# The message a delimited parser reads and an inferred one could not, in one line:
#   `看下@file(...)`        the `@` does not open the word
#   `...md)这个文件`         the value does not stop at a space, and the prose follows it
#   `@skill(...)是`         a skill's name against Chinese, not separated by anything
#   `.../a note.md)`        a path holding spaces, written as it is
#   `.../a (1) b.md)`       a path holding parentheses: the group counts them
# Spelled the way the server reads paths: it is a native program, so an MSYS
# `/c/...` is not a path it has.
spaced=$(cygpath -w "$spacey")
nested=$(cygpath -w "$nested")
message="看下@file(docs/notes/tui-main.md)这个文件，还有@skill(self-control)是做什么的，以及@file($spaced)这个、@file($nested)那个"
say "message: $message"
sent=$("${cli[@]}" send_message --session reader --message "$message" 2>&1)
say "sent: $sent"
[[ "$sent" == *'"ok":true'* ]]
ok "its turn is accepted" $?
for _ in $(seq 1 120); do grep -q 'turn end session=reader' "$log" && break; sleep 0.5; done
grep -q 'turn end session=reader outcome=Done' "$log"
ok "the turn finishes" $?

python - "$home" "$spaced" "$nested" <<'INNER'
import json, sqlite3, sys
home, spaced, nested = sys.argv[1], sys.argv[2], sys.argv[3]
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='reader'").fetchone()[0]
rows = [json.loads(d) for (d,) in c.execute(
    "select display from messages where session_id=? and kind='system' order by seq", (sid,))]
assert len(rows) == 1, f"a first turn wrote {len(rows)} prompt rows"
rooted = rows[0]["rooted"]
print("== index")
for one in rooted:
    print("  ", one)
# The `@` inside a word, and the value ending where its group does.
assert "@file(docs/notes/tui-main.md)" in rooted, rooted
# A skill's name is what stands in the group, not the sentence after it.
assert "@skill(self-control)" in rooted, rooted
# A path holding a space needs nothing written around it.
assert f"@file({spaced})" in rooted, rooted
# And one holding parentheses is read by counting them, not by stopping at the first.
assert f"@file({nested})" in rooted, rooted
# What the file itself names came along with it, so the index still walks.
assert any(one.startswith("@file(SPEC.md)") for one in rooted), rooted
texts = [json.loads(d) for (d,) in c.execute(
    "select display from messages where session_id=? and kind='text' order by seq", (sid,))]
user = [t for t in texts if t.get("role") == "user"]
assert user, texts
said = user[-1]["text"]
print("== row")
print("  ", said)
# The mention kept its spelling and gained the `SKILL.md` it names, and the prose on
# both sides of it survived.
assert "@skill(self-control) (" in said, said
assert "看下@file(docs/notes/tui-main.md)这个文件" in said, said
assert "是做什么的" in said, said
INNER
ok "a name written against prose is still a name" $?

elapsed=$(( $(date +%s) - started ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo "?")
if [[ $fails -eq 0 ]]; then
  printf 'PASS · %ss · %s\n' "$elapsed" "$sha"
else
  printf 'FAIL · %d check(s) failed · %ss · %s\n' "$fails" "$elapsed" "$sha"
  [[ "$verbose" == "-v" ]] || echo "rerun with -v, or read $log" >&2
fi
exit $(( fails > 0 ))
