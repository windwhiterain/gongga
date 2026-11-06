#!/usr/bin/env bash
# SPEC's `## Skill: Self Control`, on a real server with a fake provider: does the
# content in `ship/skills/self-control` publish the functions it names, and do the
# server behaviours its prose describes still hold?
#
#   A  `new_session` gives the new session the source's row, column by column, and
#      leaves that session empty
#   B  a name that is taken is refused, and the row it names is left as it was
#   C  a remote this server does not know is refused before a row is made
#   D  no source session is refused, and no session is made
#   E  the package publishes `ask` and `new_session`
#   F  `ask` asks, waits and prints the answer
#   G  `--dry_run` prints the arguments and makes nothing
#   H  the script form of `ask` still runs
#
# One server lifecycle, one fake endpoint, one temporary home. `--keep` leaves the
# home for a failing run; the default removes it.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/selfcontrol-home"
project="$work/selfcontrol-project"
log="$work/selfcontrol.log"
skill="$root/ship/skills/self-control"
fails=0
# The label is built before the check, never inside it: a command substitution in
# the label would reset `$?` and every check would report itself as passed.
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
# A failed prerequisite stops the run: every check after it would read a state that
# never happened. It still says FAIL first, so the transcript names what broke.
hard() {
  printf '  FAIL %s\n' "$1" >&2
  [[ -n "${log:-}" && -f "$log" ]] && tail -20 "$log" >&2
  exit 1
}

[[ -x "$bin/gongga-server.exe" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
[[ -d "$skill" ]] || { echo "no skill at $skill" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}
dial_ready() {
  python -c 'import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$1"
}
wait_tcp() { for _ in $(seq 1 100); do dial_ready "$1" && return 0; sleep 0.2; done; return 1; }
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$1" "$2" && return 0; sleep 0.2; done; return 1; }
# The servers that serve *this* home: the home is the identity, never the image
# name, because several builds and several worktrees answer to the same name.
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
# The interpreters this home started: a command layer and a skill driver are
# children of this run, and they are told apart by the home in their line.
home_pythons() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='python.exe'\" | Where-Object { \$_.CommandLine -like '*$posix*' -or \$_.CommandLine -like '*$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
query() { python - "$home" "$1" <<'INNER'
import sqlite3, sys
db = sqlite3.connect("file:%s/gongga.db?mode=ro" % sys.argv[1], uri=True)
for row in db.execute(sys.argv[2]):
    print("|".join("" if v is None else str(v) for v in row))
INNER
}
server_pid=""; fake_pid=""
cleanup() {
  for pid in $(serving_home) "$server_pid" $(home_pythons); do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/fake.pid"
  sleep 1
  [[ "${1:-}" == keep ]] && { echo "note: kept $home"; return; }
  rm -rf "$home" || echo "note: $home is still held" >&2
}
keep=""; [[ "${1:-}" == "--keep" ]] && keep=keep
trap 'cleanup "$keep"' EXIT

# A home a previous run's server still holds cannot be removed, and the run would
# then measure that old database with its `probe` rows. Stop instead of measuring it.
for pid in $(serving_home); do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
rm -rf "$home" "$project" "$log" "$work/selfcontrol-dump.jsonl"
[[ -f "$home/gongga.db" ]] && hard "a previous run still holds $home"

mkdir -p "$home" "$project"
# The content, from this checkout and not from the network: the home installs it
# at startup, and a test measures the content this branch carries.
export GONGGA_SHIP_URL="$(cygpath -m "$root/ship")"
# The remotes this server knows: none, so `--remote` names a machine it does not
# have and says so.
: > "$work/selfcontrol-remotes.toml"
export GONGGA_REMOTES_CATALOG="$work/selfcontrol-remotes.toml"
# The skill under test, installed the way a home installs it.
mkdir -p "$home/skills"
cp -r "$skill" "$home/skills/self-control"
cp -r "$root/ship/config/config.toml" "$home/config.toml" 2>/dev/null

port=$(free_port); fake_port=$(free_port)
# What the skill's commands must find: this server, this home, this key.
export GONGGA_HOME="$home"
export GONGGA_SERVER="127.0.0.1:$port"
# `gongga-cli` first: the bundle `gongga` carries a window, and a window refuses
# client-side commands.
export PATH="$home/bin:$PATH"
# The command layer, named the way a native interpreter can start it: the home
# writes a POSIX shim and a `.cmd` beside it, and a native Python cannot exec the
# first one (WinError 193), so the second is the spelling that runs here.
export GONGGA_CLI="$home/bin/gongga-cli.cmd"

export FAKE_LLM_DUMP="$work/selfcontrol-dump.jsonl"
python - "$root" "$work" "$fake_port" <<'INNER' &
import os, runpy, sys
root, work, port = sys.argv[1], sys.argv[2], sys.argv[3]
open(os.path.join(work, "fake.pid"), "w").write(str(os.getpid()))
sys.argv = [os.path.join(root, "scripts/fake_llm.py"), port]
runpy.run_path(sys.argv[0], run_name="__main__")
INNER
for _ in $(seq 1 50); do [[ -s "$work/fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake.pid" 2>/dev/null)
wait_tcp "$fake_port" || hard "the fake endpoint never answered"

cat > "$work/selfcontrol-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "FAKE_API_KEY"
# The tiers this provider publishes: a session that names one is refused unless
# the roster declares it.
cache_ttl_secs = 60
cache_ttl_tiers = { short = 60, long = 3600 }

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
  # The levels this model takes, and the one it takes when nobody says: the
  # source session is given another level, so a copy that did not copy it would
  # land on the default and say so.
  reasoning_levels = ["low", "medium", "high"]
  reasoning_default = "medium"
TOML
export GONGGA_PROVIDERS_CATALOG="$work/selfcontrol-roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening "$port" "$log" || hard "the server never listened"
server_pid=$(tr -d '\r\n' < "$home/server.lock")
cli=("$home/bin/gongga-cli.cmd")

# The row a copy has to reproduce, as the database keeps it: the model, the level
# it thinks at, the context share, the cache tier and its keepalive, the mode, the
# machine, and the directory it works in.
columns_of() {
  query "select model, thinking_effort, context_limit, cache_tier, cache_keepalive, mode, remote, cwd from sessions where name='$1'"
}
# What a session that was just made must not carry: a transcript, a pending row,
# a subscription.
empty_of() {
  query "select (select count(*) from messages where session_id=(select id from sessions where name='$1')), (select count(*) from pending_tasks where session='$1'), (select count(*) from subscriptions where session='$1')"
}
# The first line a call said, wherever it said it: a refusal is the answer, and
# the answer is on stdout or stderr depending on who wrote it.
first_line() { cat "$1" "$2" 2>/dev/null | grep -m1 . || true; }

# The project directory in the spelling a native server reads: the MSYS path is
# a different directory to it.
project_native=$(cygpath -m "$project")

# The session every copy starts from: a row with something in every column a copy
# has to reproduce, and a level the model does not take by default.
"${cli[@]}" set_session source --model fake/flash-fake --thinking high --ctx_limit 20% \
  --cache_tier short --cache_keepalive true --cwd "$project_native" >"$work/source.out" 2>&1
[[ "$(query "select count(*) from sessions where name='source'")" == "1" ]] \
  || hard "the source session was not made: $(tail -2 "$work/source.out")"
source_columns=$(columns_of source)
[[ -n "$source_columns" ]] || hard "the source session has no row"

# --- A: the copy's row is the source's row, and the copy is empty --------------
GONGGA_SESSION=source "${cli[@]}" skill self-control new_session copy >"$work/new-a.out" 2>"$work/new-a.err"
a=$?
copy_columns=$(columns_of copy)
a_label="the new session's row is the source's row: '$copy_columns' (source: '$source_columns')"
[[ -n "$copy_columns" && "$copy_columns" == "$source_columns" ]]
ok "$a_label" $?
empty=$(empty_of copy)
a_label="and it is empty: $empty (messages|pending|subscriptions)"
# A copy that is not there is not an empty one: the emptiness is only said of a
# session that exists.
[[ -n "$copy_columns" && "$empty" == "0|0|0" ]]
ok "$a_label" $?
# The call's own verdict: a session that was made and a call that says it was
# refused are two facts, and a caller reads the second one.
a_label="and the command says so: exit $a, $(first_line "$work/new-a.out" "$work/new-a.err")"
[[ $a == 0 ]]
ok "$a_label" $?

# --- B: a name that is taken is refused, and the row it names is left alone -----
"${cli[@]}" set_session taken --model fake/flash-fake --thinking low >"$work/taken.out" 2>&1
taken_before=$(columns_of taken)
GONGGA_SESSION=source "${cli[@]}" skill self-control new_session taken >"$work/new-b.out" 2>"$work/new-b.err"
b=$?
said=$(cat "$work/new-b.out" "$work/new-b.err")
b_label="a taken name is refused with the server's own words: exit $b, $(first_line "$work/new-b.out" "$work/new-b.err")"
[[ $b == 2 && "$said" == *"already exists"* ]]
ok "$b_label" $?
b_label="and the row it names is untouched: $(columns_of taken)"
[[ "$(query "select count(*) from sessions where name='taken'")" == "1" && "$(columns_of taken)" == "$taken_before" ]]
ok "$b_label" $?

# --- C: a machine this server does not know is refused before a row is made -----
GONGGA_SESSION=source "${cli[@]}" skill self-control new_session far --remote nosuch \
  >"$work/new-c.out" 2>"$work/new-c.err"
c=$?
said=$(cat "$work/new-c.out" "$work/new-c.err")
c_label="an unknown remote is refused: exit $c, $(first_line "$work/new-c.out" "$work/new-c.err")"
[[ $c == 2 && "$said" == *"no remote named"* ]]
ok "$c_label" $?
c_label="and nothing was created"
[[ "$(query "select count(*) from sessions where name='far'")" == "0" ]]
ok "$c_label" $?

# --- D: no source session is refused, and no session is made -------------------
env -u GONGGA_SESSION "${cli[@]}" skill self-control new_session orphan >"$work/new-d.out" 2>"$work/new-d.err"
d=$?
said=$(cat "$work/new-d.out" "$work/new-d.err")
d_label="no source session is refused: exit $d, $(first_line "$work/new-d.out" "$work/new-d.err")"
# The refusal is about the source: a message that names no session at all is a
# line that failed to parse, not a helper that refused to copy nothing.
lower=$(printf '%s' "$said" | tr 'A-Z' 'a-z')
[[ $d == 2 && -n "$said" && ( "$lower" == *"source"* || "$lower" == *"gongga_session"* || "$lower" == *"copy"* ) ]]
ok "$d_label" $?
d_label="and nothing was created"
[[ "$(query "select count(*) from sessions where name='orphan'")" == "0" ]]
ok "$d_label" $?

# --- E: the package publishes the two functions it names ------------------------
"${cli[@]}" skill self-control >"$work/e.out" 2>"$work/e.err"
e=$?
said=$(cat "$work/e.out" "$work/e.err")
e_label="the package publishes ask and new_session: exit $e, $(first_line "$work/e.out" "$work/e.err")"
[[ $e != 0 && "$said" == *"ask"* && "$said" == *"new_session"* ]]
ok "$e_label" $?

# --- F: `ask` asks, waits and prints the answer ---------------------------------
"${cli[@]}" set_session mentor --model fake/flash-fake --cwd "$project_native" >"$work/mentor.out" 2>&1
answer=$(timeout -k 5 120 "${cli[@]}" skill self-control ask mentor "What did I ask?" 2>"$work/ask-f.err")
f=$?
printf '%s' "$answer" > "$work/ask-f.out"
f_label="ask asks, waits and prints the answer: exit $f, '$answer' $(first_line "$work/ask-f.out" "$work/ask-f.err")"
[[ $f == 0 && "$answer" == *"hello from the fake"* ]]
ok "$f_label" $?

# --- G: a dry run prints the arguments and makes nothing ------------------------
GONGGA_SESSION=source "${cli[@]}" skill self-control new_session dryrun --dry_run true \
  >"$work/new-g.out" 2>"$work/new-g.err"
g=$?
said=$(cat "$work/new-g.out" "$work/new-g.err")
g_label="a dry run prints what it would do: exit $g, $(first_line "$work/new-g.out" "$work/new-g.err")"
[[ $g == 0 && -n "$said" && "$said" == *"dryrun"* ]]
ok "$g_label" $?
g_label="and creates nothing"
[[ "$(query "select count(*) from sessions where name='dryrun'")" == "0" ]]
ok "$g_label" $?

# --- H: the script form of `ask` still runs -------------------------------------
answer=$(timeout -k 5 120 "$home/skills/self-control/ask_session.py" mentor "What did I ask?" 2>"$work/ask-h.err")
h=$?
printf '%s' "$answer" > "$work/ask-h.out"
h_label="the script form of ask still runs: exit $h, '$answer' $(first_line "$work/ask-h.out" "$work/ask-h.err")"
[[ $h == 0 && "$answer" == *"hello from the fake"* ]]
ok "$h_label" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · self-control-skill · %s\n' "$sha"; else printf 'FAIL · self-control-skill · %s (%d)\n' "$sha" "$fails"; exit 1; fi
