#!/usr/bin/env bash
# SPEC's prompt invariant, on real processes: what a request has sent is never
# rewritten. The index carries the rules file of the session's own directory, so a
# `cd` in its shell must not take that file out of the system prompt.
#
# One line of verdict at the end; `-v` prints both prompt rows.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and a Windows program -- the
# server is one -- reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${CACHE_SELFTEST_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/prompt-home"
log="$work/prompt.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }

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
# Every process serving THIS home, whatever the run remembers. The home has several
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
  rm -f "$work/fake-prompt.pid"
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
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"
export FAKE_LLM_DUMP="${FAKE_LLM_DUMP:-$work/requests.jsonl}"
: > "$FAKE_LLM_DUMP"

export FAKE_LLM_TOOL=bash
export FAKE_LLM_TOOL_ARGS='{"command":"cd /tmp && pwd","name":"default","task":"move the shell"}'

python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/fake-prompt.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/fake-prompt.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/fake-prompt.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/fake-prompt.pid" 2>/dev/null)
if ! wait_tcp "$fake_port"; then
  echo "the fake endpoint never answered on 127.0.0.1:$fake_port" >&2
  sed -n '1,12p' "$work/fake-prompt.log" >&2
  exit 1
fi

# One model, the fake, so nothing here depends on the machine's roster.
# Two declared tiers let the fixed rule pick a short and a long one. A zero
# window keeps the keepalive inert for ordinary runs; CACHE_SELFTEST_TTL_SECS
# makes it fire in seconds for the keepalive check.
cat > "$work/prompt-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "FAKE_API_KEY"
cache_ttl_tiers = { "5m" = 300, "1h" = 3600 }
cache_ttl_secs = ${CACHE_SELFTEST_TTL_SECS:-0}

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
export GONGGA_PROVIDERS_CATALOG="$work/prompt-roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

# A session in the repository, whose rules file names SPEC.md and a skill. The model
# is named: this home's roster has one provider, and the built-in default is not it.
# The optional settings ride the same object: each adds a `,"key":"value"` tail.
session_args=""
if [[ -n "${CACHE_SELFTEST_TIER:-}" ]]; then
  session_args+=',"cache_tier":"'"$CACHE_SELFTEST_TIER"'"'
fi
if [[ -n "${CACHE_SELFTEST_KEEPALIVE:-}" ]]; then
  session_args+=',"keep_alive":"'"$CACHE_SELFTEST_KEEPALIVE"'"'
fi
created=$("${cli[@]}" '{"name":"set_session","args":{"id":"reader","model":"fake/flash-fake","cwd":"'"$(native "$root")"'"'"$session_args"'}}' 2>&1)
[[ "$created" == *'"ok":true'* ]]
ok "a session is created in the repository" $?
# The tiers a completion would offer are the tiers the roster declares: a name
# outside that list is refused here, and the refusal names the ones it has.
refused=$("${cli[@]}" '{"name":"set_session","args":{"id":"reader","cache_tier":"no-such-tier"}}' 2>&1)
[[ "$refused" == *"5m"* && "$refused" == *"1h"* ]]
ok "a tier the model does not declare is refused, listing the ones it does" $?
# The message names two references: a skill (resolved through the home) and a file by
# a *relative* path, whose meaning is the thing a `cd` used to change. A report always
# names one, which is also what keeps the index from being empty.
sent=$("${cli[@]}" '{"name":"send_message","args":{"session":"reader","message":"@skill(self-control) @file(docs/notes/tui-main.md) — move the shell, then answer"}}' 2>&1)
[[ "$sent" == *'"ok":true'* ]]
ok "its turn is accepted" $?
for _ in $(seq 1 120); do grep -q 'turn end session=reader' "$log" && break; sleep 0.5; done
grep -q 'turn end session=reader outcome=Done' "$log"
ok "a turn that cd's its shell finishes" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='reader'").fetchone()[0]
# The shell really did move: the tool the model called ran a `cd`.
moved = [json.loads(d).get("arguments", "") for (d,) in c.execute(
    "select display from messages where session_id=? and kind='tool_call'", (sid,))]
assert any("cd /tmp" in a for a in moved), moved
rows = [json.loads(d) for (d,) in c.execute(
    "select display from messages where session_id=? and kind='system' order by seq", (sid,))]
# The invariant: the shell moved, and the request that was already sent did not
# change -- so no second prompt row was written at all.
assert len(rows) == 1, f"a moved shell rewrote the prompt: {len(rows)} rows, {[r['rooted'] for r in rows]}"
only = rows[0]
assert "@file(AGENTS.md)" in only["rooted"], only["rooted"]
assert "@skill(self-control)" in only["rooted"], only["rooted"]
# A relative path means the same file after the shell moved: it is resolved in the
# session's directory, not where the last command left the shell.
assert "@file(docs/notes/tui-main.md)" in only["rooted"], only["rooted"]
# The rules file's own references came with it (AGENTS.md names SPEC.md).
assert any(root.startswith("@file(SPEC.md)") for root in only["rooted"]), only["rooted"]
INNER
ok "a cd in the shell writes no new prompt row" $?

python - "$home" <<'INNER'
import json, sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
sid = c.execute("select id from sessions where name='reader'").fetchone()[0]
prompt = json.loads(c.execute(
    "select display from messages where session_id=? and kind='system' limit 1", (sid,)).fetchone()[0])
prose, card = prompt["instructions"], prompt["tail"]
# No value in the prose at all: these bytes open every request, so a per-session
# value ends the prefix every session of a directory shares. The card states it.
assert "`reader`" not in prose, prose[:200]
assert "$GONGGA_SESSION" not in prose, prose[:200]
assert "{{" not in prose and "}}" not in prose, prose[:200]
# And the card still carries it, with the build, the first minute and the directory.
assert "Session `reader`" in card, card
assert "harness gongga at `" in card and "in `" in card, card
INNER
ok "the prose carries no value and the card names the session" $?

# The keepalive is a timer inside the server, so give it a chance to fire while
# the process is still here. Otherwise the ordinary run pays nothing.
if [[ -n "${CACHE_SELFTEST_KEEPALIVE_WAIT:-}" ]]; then
  sleep "$CACHE_SELFTEST_KEEPALIVE_WAIT"
fi

python - "$FAKE_LLM_DUMP" "${CACHE_SELFTEST_TIER:-5m}" "${CACHE_SELFTEST_KEEPALIVE:-false}" <<'INNER'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding='utf-8') if line.strip()]
want, keepalive = sys.argv[2], sys.argv[3] == "true"
if rows:
    ttls = {(r.get("prompt_cache_options") or {}).get("ttl") for r in rows}
    assert ttls == {want}, f"expected only {want}, saw {ttls}"
notices = sum(1 for r in rows if "(time notice:" in json.dumps(r, ensure_ascii=False))
if keepalive:
    assert notices >= 2, f"the heartbeat did not repeat: {notices} notice(s)"
else:
    assert notices == 0, f"a session without a heartbeat sent {notices} notice(s)"
INNER
ok "one static tier on every request, and the heartbeat follows its switch" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · prompt · %s\n' "$sha"
else
  printf 'FAIL · prompt · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
