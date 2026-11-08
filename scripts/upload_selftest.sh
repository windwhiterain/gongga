#!/usr/bin/env bash
# A picture is named by a provider file the server looks up, not one a row remembers.
#
# Three things this proves on real processes, all of which the old code got wrong:
#   - one upload per copy, however many messages name it;
#   - a file the provider has dropped is forgotten and uploaded again, and the turn
#     succeeds instead of failing for ever;
#   - the row itself never carries the id, so a transcript outlives the provider's file.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/upload-home"
log="$work/upload.log"
dump="$work/upload-dump.jsonl"
files="$work/upload-files.txt"
png="$work/upload.png"

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
  rm -f "$work/upload.pid"
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

mkdir -p "$work"; rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"; : > "$dump"; : > "$files"

python - "$png" <<'PY'
import base64, sys
open(sys.argv[1], "wb").write(base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="))
PY
[[ -s "$png" ]] || { echo "FAIL · no test picture"; exit 1; }

export FAKE_LLM_DUMP="$dump"
export FAKE_LLM_FILES_LOG="$files"
# The provider drops every file it was holding after the second request: exactly what an
# expiring file looks like to a transcript that recorded the id instead of the copy.
export FAKE_LLM_FORGET_FILES_AFTER=2
python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/upload.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/upload-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/upload.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/upload.pid" 2>/dev/null)
wait_tcp "$fake_port" || { echo "FAIL · the fake endpoint never answered"; exit 1; }

cat > "$work/upload-roster.toml" <<TOML
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
  modalities = ["text", "image"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/upload-roster.toml"
export FAKE_API_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && break; sleep 0.2; done
grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || { echo "FAIL · the server never listened"; exit 1; }

cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")
created=$("${cli[@]}" set_session --name viewer --model fake/flash-fake --cwd "$work" 2>&1)
[[ "$created" == *'"ok":true'* ]] || { echo "FAIL · session not created: $created" >&2; exit 1; }

posix_png=$(cygpath -m "$png" 2>/dev/null || echo "$png")
turns=0
send() {
  "${cli[@]}" send_message --name viewer --message "$1" >/dev/null 2>&1
  turns=$((turns + 1))
  for _ in $(seq 1 120); do
    [[ $(grep -c 'turn end session=viewer' "$log") -ge $turns ]] && break
    sleep 0.5
  done
}
send "first look at @image($posix_png) please"
after_first=$(wc -l < "$files")
send "the same picture again: @image($posix_png)"
after_second=$(wc -l < "$files")
# The provider drops everything it holds once this many requests have been served, so
# the third turn is the one that meets a dead file.
send "and a third time @image($posix_png)"
after_third=$(wc -l < "$files")

ok "three turns with the same picture ran" \
  $(( $(grep -c 'turn end session=viewer' "$log") >= 3 ? 0 : 1 ))
# The same picture named again is not uploaded again.
ok "a repeated picture was not uploaded twice" \
  $(( after_first >= 1 && after_second == after_first ? 0 : 1 ))

python - "$home" "$dump" "$files" "$log" "$after_first" "$after_third" <<'INNER'
import json, sqlite3, sys
home, dump, files, log = sys.argv[1:5]
before_recovery = int(sys.argv[5])
after_recovery = int(sys.argv[6])
uploads = [line.strip() for line in open(files, encoding="utf-8") if line.strip()]
bodies = [json.loads(line) for line in open(dump, encoding="utf-8") if line.strip()]

# 1. The row records the copy and never a provider id: a transcript outlives the file.
c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
rows = [json.loads(d) for (d,) in c.execute("select display from messages where kind='text'")]
named = [r for r in rows if r.get("images")]
assert named, "no message recorded a picture"
for row in named:
    for image in row["images"]:
        assert image.get("copy") is not None, f"a picture with no copy: {image}"
        assert "file_id" not in image, f"a row recorded a provider id: {image}"

# 2. Every upload minted a distinct id, and the first two turns reused one.
assert len(set(uploads)) == len(uploads), "the same file was uploaded twice under one id"
assert before_recovery >= 1, "nothing was ever uploaded"

# 3. The provider dropped its files mid-conversation, and the turn still went through:
#    the id was forgotten and the picture uploaded again.
recovered = sum(1 for line in open(log, encoding="utf-8") if "its ids were forgotten" in line)
assert recovered >= 1, "a dead file id was never recovered"
assert after_recovery > before_recovery, "no re-upload followed the provider dropping its files"

# 4. And every request after the drop named a file the provider was still holding.
assert any("file-fake-" in json.dumps(b) for b in bodies), "no request named a file"
INNER
ok "the row holds the copy, uploads are reused, and a dead id recovers" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · upload · %s\n' "$sha"
else printf 'FAIL · upload �� %d scenario(s) · %s\n' "$fails" "$sha"; exit 1; fi
