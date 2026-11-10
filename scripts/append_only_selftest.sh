#!/usr/bin/env bash
# A reference file changing must not rewrite a request that was already sent.
#
# One turn, then a second one after `AGENTS.md` has grown: the head must be the same
# bytes, and the second body must extend the first. This is the failure that cost a
# whole transcript every time an indexed file was edited -- the index lived in the
# system prompt and was re-rendered at every step.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/append-only-home"
log="$work/append-only.log"
dump="$work/append-only-dump.jsonl"

fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build --workspace" >&2; exit 1; }

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
  rm -f "$work/append-only.pid"
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

mkdir -p "$work"
rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"

export FAKE_LLM_DUMP="$dump"
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/append-only.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/append-only-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/append-only.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/append-only.pid" 2>/dev/null)
wait_tcp "$fake_port" || { echo "the fake endpoint never answered" >&2; exit 1; }

cat > "$work/append-only-roster.toml" <<TOML
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
export GONGGA_PROVIDERS_CATALOG="$work/append-only-roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
# Listening, not merely locked: a lock is written before the socket is bound, and a
# client that dials in between retries for a minute and then gives up silently.
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; exit 1; }
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

created=$("${cli[@]}" set_session --id reader --model fake/flash-fake --cwd "$root" 2>&1)
[[ "$created" == *'"ok":true'* ]] || { echo "FAIL · session not created: $created" >&2; exit 1; }
"${cli[@]}" send_message --session reader --message "read @file(docs/notes/tui-main.md) then answer" >/dev/null 2>&1
for _ in $(seq 1 120); do grep -q 'turn end session=reader' "$log" && break; sleep 0.5; done
first=$(grep -c 'turn end session=reader' "$log")

# The reference the index carries changes, and is *named again* so a fresh plan sees it.
printf '\n- a line added while the session is running\n' >> "$root/AGENTS.md"
restore=$?
"${cli[@]}" send_message --session reader --message "and again @file(docs/notes/tui-main.md)" >/dev/null 2>&1
for _ in $(seq 1 120); do [[ $(grep -c 'turn end session=reader' "$log") -gt $first ]] && break; sleep 0.5; done
git -C "$root" checkout -- AGENTS.md

ok "two turns ran" $(( first > 0 && $(grep -c 'turn end session=reader' "$log") > first ? 0 : 1 ))

python - "$home" "$dump" <<'INNER'
import json, sqlite3, sys
home, dump = sys.argv[1], sys.argv[2]
# The dump keeps each request as the JSON the fake decoded; re-serializing in order
# gives back the bytes that matter here -- the head, then the items in sequence.
bodies = [
    json.dumps(json.loads(line), separators=(",", ":"), ensure_ascii=False)
    for line in open(dump, encoding="utf-8") if line.strip()
]
assert len(bodies) >= 2, f"only {len(bodies)} requests"
# The head of every request is the same bytes: it was stamped once.
heads = [b[: b.index('"input":[')] for b in bodies]
assert len(set(heads)) == 1, f"the head moved between requests: {[len(h) for h in heads]}"
# And each body extends the one before it, so the provider's cached prefix holds.
prev = bodies[0].rstrip("]}")
for body in bodies[1:]:
    assert body.startswith(prev), "a later request is not an extension of the earlier one"
    prev = body.rstrip("]}")
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
rows = c.execute("select count(*) from messages where kind='system'").fetchone()[0]
assert rows == 1, f"the head was written {rows} times"
# The added line is carried, appended, rather than by rewriting what was sent.
# And the point of all of it: the edit did not buy a rewritten prefix. A session that
# indexed a file keeps the text it indexed; the price is stated rather than paid in
# cache misses. What the edit must not do is move the head -- asserted above.
assert not any("a line added while the session is running" in b for b in bodies), \
    "the edit rewrote a request that was already sent"
INNER
ok "the head never moves and each body extends the last" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · append-only · %s\n' "$sha"
else printf 'FAIL · append-only · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1; fi
