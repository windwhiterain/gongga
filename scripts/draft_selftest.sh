#!/usr/bin/env bash
# Half a message is still a message. A stream killed mid-answer leaves the chunks the
# reader already saw, as a row marked `interrupted`, at the next start.
#
# The alternative was the state before this: the text lived in one process's memory, so
# a kill lost an answer the window had already drawn.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/draft-home"
log="$work/draft.log"
dump="$work/draft-dump.jsonl"

fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
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
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

port=$(free_port); fake_port=$(free_port); fake_pid=""
cleanup() {
  for pid in $(serving_home); do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -f "$work/draft.pid"
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

mkdir -p "$work"; rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"

# Three pieces, a second apart: a kill between them lands mid-answer.
export FAKE_LLM_DUMP="$dump"
export FAKE_LLM_SLOW_PIECES='the first half of an answer|the second half|the ending'
# A thought streams first, so the kill can land inside one.
export FAKE_LLM_SLOW_REASONING='I should think about|the first half of this'
export FAKE_LLM_SLOW_PAUSE=1.2
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/draft.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/draft-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/draft.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/draft.pid" 2>/dev/null)
wait_tcp "$fake_port" || { echo "FAIL · the fake endpoint never answered"; exit 1; }

cat > "$work/draft-roster.toml" <<TOML
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
export GONGGA_PROVIDERS_CATALOG="$work/draft-roster.toml"
export FAKE_API_KEY=sk-fake

start() {
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$1" 2>&1 &
  for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$1" && break; sleep 0.2; done
}

start "$log"
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; exit 1; }
cli=("$bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port")
"${cli[@]}" set_session --session writer --cwd "$work" --model fake/flash-fake >/dev/null 2>&1
"${cli[@]}" send_message --session writer --message "say something long" >/dev/null 2>&1

# Both a thought and an answer have to be in flight before the kill: that is what makes
# each one's survival a separate fact rather than one scenario standing for two.
for _ in $(seq 1 150); do
  python - "$home" <<'INNER' && break
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
parts = {p for (p,) in c.execute("select distinct part from drafts where text <> ''")}
sys.exit(0 if {"reasoning", "output"} <= parts else 1)
INNER
  sleep 0.2
done
drafted=$(python - "$home" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
print(",".join(sorted(p for (p,) in c.execute(
    "select distinct part from drafts where text <> ''"))))
INNER
)
[[ "${drafted}" == *reasoning* && "${drafted}" == *output* ]]
ok "a thought and an answer are both drafted as they stream" $?

# The server runs under a watchdog, so killing the process that answers is not a kill:
# the watchdog restarts it and the "restart" never happens. The tree has to go, and the
# supervisor's pid is the one the server says it runs under.
pid=$(cat "$home/server.lock")
supervisor=$(sed -n 's/.*supervisor=\([0-9]*\).*/\1/p' "$log" | tail -1)
taskkill //PID "${supervisor:-$pid}" //T //F >/dev/null 2>&1
taskkill //PID "$pid" //F >/dev/null 2>&1
for _ in $(seq 1 50); do
  alive "$pid" || break
  sleep 0.2
done
sleep 1

: > "$log"
start "$log"
grep -q "half-answers kept before serving" "$log"
ok "the next start keeps it" $?

python - "$home" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
rows = [r[0] for r in c.execute(
    "select display from messages where kind='text' and display like '%interrupted%'")]
assert rows, "the half answer is not in the transcript"
import json
kept = json.loads(rows[0])
assert kept.get("interrupted") is True, kept
assert kept["text"].strip(), "the row is empty"
assert "first half" in kept["text"], kept["text"]
# The draft that became that row is gone. A *new* turn replays the pending work and
# streams an answer of its own -- with the same item id and the same words, because the
# fake is deterministic -- so presence is not the test; age is. Nothing from before the
# row may still be a draft.
kept_at = c.execute(
    "select created_at from messages where kind='text' and display like '%interrupted%'"
    " order by seq desc limit 1").fetchone()[0]
stale = c.execute(
    "select count(*) from drafts where updated_at < ?", (kept_at,)).fetchone()[0]
assert stale == 0, f"{stale} draft(s) predate the row they became"
INNER
ok "it is a row marked interrupted, and the draft is gone" $?

# Half a *thought* is kept as a reasoning row, because a thought the model wrote is
# context it should read back. Two things have to hold for that to be sendable: the
# stored item is the shape measured as accepted (text, no blob), and it sits after the
# last output -- never between a call and its own output.
python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db:ro".replace(":ro", "?mode=ro"),
                    uri=True)
rows = list(c.execute("select seq, kind, display, content from messages order by seq"))
half = [json.loads(d) for s, k, d, _ in rows if k == "reasoning" and "interrupted" in d]
assert half, "the half thought was not kept as a reasoning row"
assert not half[0].get("encrypted"), half[0]
assert half[0]["summary"].strip(), half[0]
# The wire item is what the provider was measured to accept: text and no blob.
wire = json.loads(next(cl for s, k, d, cl in rows
                       if k == "reasoning" and "interrupted" in d and cl))
assert wire["type"] == "reasoning", wire
assert wire["summary"] == [{"type": "summary_text", "text": half[0]["summary"]}], wire
assert "encrypted_content" not in wire, wire
# And its position: after the last tool output, never between a call and its output.
last_output = max((s for s, k, _, _ in rows if k == "tool_result"), default=0)
half_at = next(s for s, k, d, _ in rows if k == "reasoning" and "interrupted" in d)
assert half_at > last_output, (half_at, last_output)
INNER
ok "the half thought is a reasoning row, in a position the provider accepts" $?

# A finished answer leaves no draft behind: the normal path is unchanged.
"${cli[@]}" send_message --session writer --message "and now a whole one" >/dev/null 2>&1
for _ in $(seq 1 100); do grep -q "turn end session=writer" "$log" && break; sleep 0.3; done
python - "$home" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
left = c.execute("select count(*) from drafts").fetchone()[0]
assert left == 0, f"{left} draft(s) left after a finished answer"
INNER
ok "a finished answer leaves no draft" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · draft · %s\n' "$sha"
else printf 'FAIL · draft · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1; fi
