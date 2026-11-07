#!/usr/bin/env bash
# SPEC's `## Skill: subagent`, on a real server with a fake provider: does the
# content in `ship/skills/subagent` do the four things it names, and do the server
# behaviours its prose describes still hold?
#
#   0  the head is stamped with the session's first message, and only that
#   A  spawn creates a session under the caller's name, and arms `turn_end` for it
#   B  a taken name becomes a new segment, never somebody else's transcript
#   C  a multi-segment role is refused, and nothing is created
#   D  no `GONGGA_SESSION` is refused
#   E  a caller that is not a session is refused, and nothing is left behind
#   F  the child's brief keeps its `@file:`/`@skill:` spellings, in its first row
#   G  the child's turn end reaches the parent's transcript as a subscription notice
#   H  a later message's reference does not join the stamped head
#   I  the skill is a package: its API is `spawn(role, ...)`, run by `skill subagent spawn`
#   K  deleting a session takes its own subscriptions, not its children's
#
# One server lifecycle, one fake endpoint, one temporary home. `--keep` leaves the
# home for a failing run; the default removes it.
set -uo pipefail
# This run owns its session names. An agent's shell exports its own `GONGGA_SESSION`,
# and a script that inherits it asks *this* home's server about a session it has
# never heard of -- which reads as a broken `skill ls`, not as a leaked variable.
unset GONGGA_SESSION

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/subagent-home"
project="$work/subagent-project"
log="$work/subagent.log"
skill="$root/ship/skills/subagent"
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
# A failed prerequisite stops the run: every check after it would read a state that
# never happened. It still says FAIL first, so the transcript names what broke.
hard() {
  printf '  FAIL %s\n' "$1" >&2
  [[ -n "${log:-}" && -f "$log" ]] && tail -20 "$log" >&2
  exit 1
}

[[ -x "$bin/gongga-server.exe" && -x "$home/bin/gongga-cli" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }

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
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
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
  for pid in $(serving_home) "$server_pid"; do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  alive "${wait_fake_pid:-}" && taskkill //PID "$wait_fake_pid" //F >/dev/null 2>&1
  rm -f "$work/wait-fake.pid"
  rm -f "$work/fake.pid"
  sleep 1
  [[ "${1:-}" == keep ]] && { echo "note: kept $home"; return; }
  rm -rf "$home" || echo "note: $home is still held" >&2
}
keep=""; [[ "${1:-}" == "--keep" ]] && keep=keep
trap 'cleanup "$keep"' EXIT

# A home a previous run's server still holds cannot be removed, and the run would
# then measure that old database with its `probe` rows. Stop instead of measuring it.
for pid in $(serving_home) "$server_pid"; do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
rm -rf "$home" "$project" "$log" "$work/subagent-dump.jsonl"
[[ -f "$home/gongga.db" ]] && hard "a previous run still holds $home"

mkdir -p "$home" "$project" "$project/.agents/skills/late"
# The content repo, seeded from this checkout: startup never touches the network.
# A submodule's .git is a file pointing at the outer repo; give the copy its own.
cp -r "$root/ship" "$home/ship" 2>/dev/null
if [[ -f "$root/.git/modules/ship/HEAD" ]]; then
  mkdir -p "$home/ship"
  cp -r "$root/.git/modules/ship" "$home/ship/.git-real"
  printf 'gitdir: .git-real\n' > "$home/ship/.git"
  # The copied config still points at the outer checkout's worktree; git itself
  # cannot run against it, so the file is edited where it stands.
  sed -i '/^\tworktree = /d' "$home/ship/.git-real/config"
fi
printf 'EARLY file named by the brief.\n' > "$project/early.md"
printf 'LATE file named by a later message.\n' > "$project/late.md"
cat > "$project/.agents/skills/late/SKILL.md" <<'EOF'
---
name: late
description: A skill only a later message names.
---

LATE skill body marker.
EOF
# The skill under test, installed the way a home installs it: `entry` runs from here.
mkdir -p "$home/skills"
cp -r "$skill" "$home/skills/subagent"
cp -r "$root/ship/config/config.toml" "$home/config.toml" 2>/dev/null

port=$(free_port); fake_port=$(free_port); wait_port=$(free_port)
# What `spawn` runs must find *this* server and *this* key, not the machine's own:
# `gongga-cli` resolves both from `GONGGA_SERVER` / `GONGGA_HOME` before any default.
export GONGGA_HOME="$home"
export GONGGA_SERVER="127.0.0.1:$port"
# `spawn` finds `gongga-cli` on PATH: it must be this build, not an installed one.
export PATH="$home/bin:$bin:$PATH"
export FAKE_LLM_TOOL=bash
export FAKE_LLM_TOOL_ARGS='{"command":"echo hi","name":"default"}'
export FAKE_LLM_DUMP="$work/subagent-dump.jsonl"
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
# The endpoint whose first request asks the model to `wait`.
start_wait_fake() {
  alive "${wait_fake_pid:-}" && taskkill //PID "$wait_fake_pid" //F >/dev/null 2>&1
  rm -f "$work/wait-fake.pid"
  env FAKE_LLM_TOOL=wait FAKE_LLM_TOOL_ARGS='{"timeout_ms": 2500}' \
    python - "$root" "$work" "$wait_port" <<'INNER' &
import os, runpy, sys
root, work, port = sys.argv[1], sys.argv[2], sys.argv[3]
open(os.path.join(work, "wait-fake.pid"), "w").write(str(os.getpid()))
sys.argv = [os.path.join(root, "scripts/fake_llm.py"), port]
runpy.run_path(sys.argv[0], run_name="__main__")
INNER
  for _ in $(seq 1 50); do [[ -s "$work/wait-fake.pid" ]] && break; sleep 0.2; done
  wait_fake_pid=$(cat "$work/wait-fake.pid" 2>/dev/null)
  wait_tcp "$wait_port" || hard "the wait endpoint never answered"
}
start_wait_fake

cat > "$work/subagent-roster.toml" <<TOML
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

# A second endpoint, whose fake calls `wait` on the first request of its own process:
# the one request in a run that opens by waiting for news.
[[provider]]
id = "waitfake"
name = "Wait Fake"
key_env = "WAIT_API_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$wait_port/v1"

  [[provider.model]]
  id = "wait-fake"
  name = "Wait Fake"
  tier = "flash"
  context = 100000
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/subagent-roster.toml"
export FAKE_API_KEY=sk-fake
export WAIT_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening "$port" "$log" || hard "the server never listened"
server_pid=$(tr -d '\r\n' < "$home/server.lock")
cli=("$home/bin/gongga-cli")

# The name is matched whole: `probe` must not count `probe/impl`'s turns.
await() { local n; for _ in $(seq 1 240); do n=$(grep -cE "turn end session=$1( |$)" "$log"); [[ $n -ge $2 ]] && return 0; sleep 0.5; done; echo "  note: $1 has $n turn end(s), wanted $2" >&2; return 1; }
turns_of() { grep -cE "turn end session=$1( |$)" "$log"; }

"${cli[@]}" set_session probe --model fake/flash-fake --cwd "$project" --ctx_limit 20% >"$work/parent.out" 2>&1

# The parent speaks first, and this one message is what its head will carry for
# ever: `prompt_in_force` writes prose and index once and returns the row after.
"${cli[@]}" send_message --name probe 'Start here with @file(early.md) please.' >"$work/parent-first.out" 2>&1
await probe 1 || hard "the parent's first turn never ended"
rooted=$(query "select json_extract(display,'\$.rooted') from messages where session_id=(select id from sessions where name='probe') and kind='system'")
[[ "$rooted" == '["@file(early.md)"]' ]]
ok "0 the parent's head is stamped with its first message's references: $rooted" $?

# --- A: one spawn, one session under the caller's name, one subscription ---------
name=$(GONGGA_SESSION=probe "${cli[@]}" skill subagent spawn impl --cwd "$project" --model fake/flash-fake --context_limit 20% 2>"$work/spawn-a.err")
[[ "$name" == "probe/impl" ]]; ok "A spawn names the session <caller>/<role>: $name" $?
armed=$(query "select session, pattern, persistent from subscriptions")
[[ "$armed" == 'probe|^turn_end:probe/impl$|1' ]]
ok "A and arms ^turn_end:probe/impl\$ for probe, persistently: $armed" $?
[[ "$(query "select count(*) from sessions where name='probe/impl'")" == "1" ]]
ok "A the session exists once" $?
taken_ctx=$(query "select context_limit from sessions where name='probe/impl'")
[[ "$taken_ctx" == "20" ]]
ok "A its --context_limit was taken: $taken_ctx" $?

# --- B: the same role twice is a new segment, never a shared transcript ----------
GONGGA_SESSION=probe "${cli[@]}" skill subagent spawn impl --model fake/flash-fake \
  >"$work/spawn-b.out" 2>"$work/spawn-b.err"
second=$(cat "$work/spawn-b.out")
# The label is built before the check, never inside it: a command substitution
# among `ok`'s arguments runs first and leaves its own status where the check's
# was, so every check below reads `$?` into a variable and prints that.
b_said=$(head -2 "$work/spawn-b.err" | tr '\n' ' ')
[[ "$second" == "probe/impl/2" ]]
ok "B a taken name becomes a new segment: $second $b_said" $?
[[ "$(query "select count(*) from subscriptions where session='probe'")" == "2" ]]
ok "B and it is separately subscribed" $?

# --- C and D: the two refusals ------------------------------------------------
GONGGA_SESSION=probe "${cli[@]}" skill subagent spawn a/b >/dev/null 2>"$work/spawn-c.err"; c=$?
c_said=$(head -1 "$work/spawn-c.err")
[[ $c == 2 && "$(grep -c 'one segment' "$work/spawn-c.err")" -ge 1 ]]
ok "C a multi-segment role is refused: $c_said" $?
[[ "$(query "select count(*) from sessions where name like 'probe/a%'")" == "0" ]]
ok "C and nothing is created" $?
env -u GONGGA_SESSION "${cli[@]}" skill subagent spawn lost >/dev/null 2>"$work/spawn-d.err"; d=$?
d_said=$(head -1 "$work/spawn-d.err")
[[ $d == 2 && "$(grep -c 'GONGGA_SESSION is not set' "$work/spawn-d.err")" -ge 1 ]]
ok "D no GONGGA_SESSION is refused: $d_said" $?

# --- E: a caller that is not a session is refused, and nothing is left ----------
# The child's configuration is copied from the caller's own row, so a caller that
# is no session has nothing to copy from: the first step is refused, before any
# session exists. What this check still asserts is the property it always did --
# nothing half-made is left behind. (The create-then-subscribe rollback has no
# reachable trigger left: with the caller's row present, only the server can make
# `subscribe` fail, and `spawn` names every argument it passes.)
GONGGA_SESSION=ghost "${cli[@]}" skill subagent spawn lost --model fake/flash-fake >/dev/null 2>"$work/spawn-e.err"; e=$?
e_said=$(tail -1 "$work/spawn-e.err")
[[ $e == 2 && "$(grep -c 'no session named' "$work/spawn-e.err")" -ge 1 ]]
ok "E a caller that is not a session is refused: $e_said" $?
[[ "$(query "select count(*) from sessions where name='ghost/lost'")" == "0" ]]
ok "E and nothing was created" $?

# --- F and G: the brief, and the child's turn end reaching the parent ----------
"${cli[@]}" send_message --name probe/impl \
  'Implement @file(early.md), following @skill(late). Answer done.' >"$work/brief.out" 2>&1
await probe/impl 1 || hard "the child's turn never ended"
kept=$(query "select json_extract(display,'\$.text') from messages where session_id=(select id from sessions where name='probe/impl') and kind='text' and display like '%@file%'" | head -1)
[[ "$kept" == *"@file(early.md)"* && "$kept" == *"@skill(late)"* ]]
ok "F the brief keeps both spellings in its stored row" $?
child_index=$(query "select length(json_extract(display,'\$.index')) from messages where session_id=(select id from sessions where name='probe/impl') and kind='system'")
[[ "${child_index:-0}" -gt 100 ]]
ok "F and the child's index carries their bodies: $child_index bytes" $?
await probe 2 || hard "the notice never woke the parent"
notice=$(query "select json_extract(display,'\$.text') from messages where session_id=(select id from sessions where name='probe') and kind='text' and display like '%turn_end probe/impl:%'")
# The server speaks for no subscription: a match runs the row's command, and for a
# subagent that command is `send_message` carrying `$GONGGA_EVENT_*` (222847e).
[[ "$notice" == *'turn_end probe/impl: '* && "$notice" == *'"outcome":"done"'* ]]
ok "G the child's turn end is a message in the parent's transcript" $?

# --- H: a reference named later does not join the stamped head ------------------
# Counted before the send: this parent answers in milliseconds, so a count taken
# after it may already include the turn being waited for.
before=$(turns_of probe)
"${cli[@]}" send_message --name probe \
  'Also read @file(late.md) and @skill(late), then stop.' >"$work/later.out" 2>&1
await probe $(( before + 1 )) || hard "the parent's second turn never ended"
later=$(query "select json_extract(display,'\$.text') from messages where session_id=(select id from sessions where name='probe') and kind='text' and display like '%late.md%'")
[[ "$later" == *"@file(late.md)"* && "$later" == *"@skill(late) ("*"late"*"SKILL.md)"* ]]
ok "H a later message keeps @file(...) and expands @skill(...) to its path" $?
python - "$work/subagent-dump.jsonl" <<'INNER'
import json, sys
last = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()][-1]
# The head is the prose, the index and the card; the card is where it ends. The
# index travels as a message, never in the system text (docs/reference/prefix-cache.md),
# so reading `instructions` alone would miss the very thing this checks.
head = [last.get("instructions") or ""]
for message in last.get("input") or []:
    text = json.dumps(message.get("content"), ensure_ascii=False)
    if "Session `probe`" in text:
        break
    head.append(text)
head = "\n".join(head)
carried = [m for m in ["EARLY file named", "LATE file named", "LATE skill body"] if m in head]
print("      last request's head carries:", carried)
sys.exit(0 if carried == ["EARLY file named"] else 1)
INNER
ok "H so the head still carries only what the first message named" $?
[[ "$(query "select count(*) from messages where session_id=(select id from sessions where name='probe') and kind='system'")" == "1" ]]
ok "H and one request later there is still exactly one head row" $?

# --- J: the pattern is anchored, so a grandchild's turn is not the grandparent's --
# `probe` hears about `probe/impl`; `probe/impl/review` ends with `turn_end` about a
# session whose name *starts with* that pattern's. Unanchored, `probe` would hear it.
grand=$(GONGGA_SESSION=probe/impl "${cli[@]}" skill subagent spawn review --model fake/flash-fake 2>/dev/null)
[[ "$grand" == "probe/impl/review" ]]
ok "J the caller is the parent, so a grandchild is <child>/<role>: $grand" $?
"${cli[@]}" send_message --name "$grand" 'Answer with one word.' >"$work/grand.out" 2>&1
await "$grand" 1 || hard "the grandchild's turn never ended"
sleep 1
[[ "$(query "select count(*) from messages where session_id=(select id from sessions where name='probe/impl') and kind='text' and display like '%turn_end probe/impl/review:%'")" == "1" ]]
ok "J its own parent is told exactly once" $?
[[ "$(query "select count(*) from messages where session_id=(select id from sessions where name='probe') and kind='text' and display like '%turn_end probe/impl/review:%'")" == "0" ]]
ok "J and the grandparent is not told at all" $?

# --- K: deleting a session takes its own subscriptions, not its children's -------
"${cli[@]}" del_session probe/impl >"$work/del.out" 2>&1
[[ "$(query "select count(*) from subscriptions where session='probe/impl'")" == "0" ]]
ok "K the deleted session's subscriptions go with it" $?
kept=$(query "select group_concat(pattern,' ') from subscriptions where session='probe'")
[[ "$(query "select count(*) from subscriptions where session='probe'")" == "2" ]]
ok "K the parent's own two stay: $kept" $?
[[ "$(query "select count(*) from sessions where name='probe/impl/review'")" == "1" ]]
ok "K and its child session is still there, now unheard" $?

# --- L: `wait` waits for the NEXT thing, and no subscription means it sits ------
# 2.5 s is the bound the fake asks for, so a timeout is 2.5 s of wall clock and an
# early answer is not: the two are told apart by time as well as by the words.
"${cli[@]}" set_session alone --model waitfake/wait-fake >"$work/alone.out" 2>&1
started=$(date +%s%3N)
"${cli[@]}" send_message --name alone 'open by waiting' >"$work/alone-msg.out" 2>&1
await alone 1 || hard "the waiting session's turn never ended"
spent=$(( $(date +%s%3N) - started ))
# The last result of that session, as one value: an `output` is multi-line, so a
# line of the answer is not the answer.
waits=$(query "select json_extract(display,'\$.output') from messages where session_id=(select id from sessions where name='alone') and kind='tool_result' order by seq desc limit 1")
[[ "$waits" == *"nothing arrived within"* && $spent -ge 2500 ]]
ok "L with no subscription, wait sits to its timeout: $spent ms, '$waits'" $?

# --- M: a spawned child's turn end is what interrupts that same wait -------------
# The parent has to be busy for the notice to be a queued row: it is inside its
# first `wait` when the child finishes, so the tool returns early instead of timing out.
start_wait_fake
child_from_alone=$(GONGGA_SESSION=alone "${cli[@]}" skill subagent spawn impl --model waitfake/wait-fake 2>/dev/null)
[[ "$child_from_alone" == "alone/impl" ]]; ok "M a second parent gets its own child: $child_from_alone" $?
"${cli[@]}" send_message --name alone 'wait while the child works' >"$work/alone-wait.out" 2>&1
for _ in $(seq 1 100); do
  [[ $(query "select count(*) from messages where session_id=(select id from sessions where name='alone') and kind='tool_call'") -ge 2 ]] && break
  sleep 0.2
done
before=$(turns_of alone)
"${cli[@]}" send_message --name "$child_from_alone" 'answer at once' >"$work/child-quick.out" 2>&1
await "$child_from_alone" 1 || hard "the child's turn never ended"
await alone $(( before + 1 )) || hard "the parent's waiting turn never ended"
last=$(query "select json_extract(display,'\$.output') from messages where session_id=(select id from sessions where name='alone') and kind='tool_result' order by seq desc limit 1")
# A callback's `send_message` is both a pending row and the inbox signal, so the
# answer names whichever the wait saw first; what M measures is that it came back.
[[ "$last" != *"nothing arrived"* && "$last" == *"next step boundary"* ]]
ok "M the child's turn end interrupts it: '$last'" $?

# --- I: the skill is a package whose API is `spawn(role, ...)` -----------------
here=$("${cli[@]}" skill ls 2>/dev/null | awk -F'\t' '$1=="subagent"{print $4}')
[[ "$here" == *"subagent"* ]]
ok "I skill ls gives the SKILL.md the package sits beside: $here" $?
asked=$(env -u GONGGA_SESSION "${cli[@]}" skill run subagent 2>&1)
[[ "$asked" == *"publishes: spawn"* ]]
ok "I the package publishes one function, spawn" $?
wrong=$(env -u GONGGA_SESSION "${cli[@]}" skill run subagent spawn 2>&1)
[[ "$wrong" == *"spawn(role"*"needs role"* ]]
ok "I and calling it without a role is refused with its signature" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · subagent-skill · %s\n' "$sha"; else printf 'FAIL · subagent-skill · %s (%d)\n' "$sha" "$fails"; exit 1; fi
