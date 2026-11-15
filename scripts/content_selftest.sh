#!/usr/bin/env bash
# What a home reads out of its content checkout: the files themselves, in place, and
# what happens to the copies an older build installed beside them.
#
# There is no install pass any more: a home's config, its skills and its Python surface
# *are* the checkout's files, so an edit there is an edit here -- and a copy of them,
# left in the home by an older build, is cleaned away when its bytes are the content's
# and kept (and named) when they are not. This check builds a content repository of its
# own, moves it, edits it under a running server, and seeds a home the way the older
# build left it.
#
# `GONGGA_SHIP_CONTENT` names a checkout to take the home's `config/config.toml` from (a
# home whose config says nothing about its prompts refuses to start); it defaults to
# this checkout's `ship`, which a linked worktree does not have.
# One line of verdict at the end; `-v` prints what each scenario saw.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$root/scripts/selftest_bin.sh"
bin=$(selftest_bin "$root") || exit 1
# One run, one directory. Two of these at once -- which is how a fix is checked
# against the bug -- would otherwise clear each other's scratch home, and the symptom
# of that is a repository that vanishes mid-clone, not a queue.
mkdir -p "$root/.dev"
work=$(mktemp -d "$root/.dev/content-XXXXXX") || exit 1
home="$work/home"
log="$work/content.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { # name, condition-result
  if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi
}

server_bin="$bin/gongga-server"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"

protocol=$(awk -F' = ' '$1 == "protocol" { print $2 }' "$root/crates/server/ship.toml" | sed 's/[[:space:]]*$//')
[[ -n "$protocol" ]] || { echo "no protocol in $root/crates/server/ship.toml" >&2; exit 1; }
from=${GONGGA_SHIP_CONTENT:-$root/ship}
[[ -f "$from/config/config.toml" ]] || {
  echo "no content checkout to take a config from: set GONGGA_SHIP_CONTENT" >&2; exit 1; }
content="$work/content-content"
version() { # name, description, body
  printf -- '---\nname: %s\ndescription: %s\n---\n\n%s\n' "$1" "$2" "$3"
}
commit() { git -C "$content" add -A && git -C "$content" commit -qm "$1" && git -C "$content" rev-parse HEAD; }
seed_content() {
  rm -rf "$content"; mkdir -p "$content/skills/alpha" "$content/skills/beta" "$content/config" "$content/python"
  git -C "$content" init -q
  git -C "$content" branch -m main
  git -C "$content" config user.email selftest@example.invalid
  git -C "$content" config user.name selftest
  cat > "$content/ship.toml" <<TOML
protocol = $protocol

config = "config/config.toml"

python = "python"

[skills]
harness = "skills"
TOML
  cp "$from"/python/*.py "$content/python/"
  cp "$from/config/config.toml" "$content/config/config.toml"
  version alpha "v1 handoff" "body one" > "$content/skills/alpha/SKILL.md"
  version beta "v1 remote" "body one" > "$content/skills/beta/SKILL.md"
  commit 'content v1' >/dev/null
}
seed_content

free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
# Every process serving THIS home: identity is the home in the command line, never the
# image name -- another agent's server has the same name as this one.
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

server_pid=""
sup=""
cleanup() {
  local pid
  for pid in $(serving_home) "${server_pid:-}" "${sup:-}"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  if ! rm -rf "$work" 2>/dev/null; then
    sleep 3
    rm -rf "$work" 2>/dev/null || echo "note: $work is still held by a process" >&2
  fi
}
trap cleanup EXIT

# One server per scenario: the checkout is prepared at startup, and the state a
# scenario reads is the state seeded just before it. `stop` leaves nothing.
start() {
  : > "$log"
  port=$(free_port)
  GONGGA_SHIP_URL="$content" env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
  for _ in $(seq 1 150); do grep -q "reads the checkout in place" "$log" && break; sleep 0.2; done
  if [[ ! -s "$home/server.lock" ]]; then
    cp "$log" "$log.failed"
    echo "serving this home: $(serving_home)" >&2
  fi
  server_pid=$(child_pid); sup=$(parent_of "$server_pid")
  report=$(grep -o 'content: this home reads the checkout in place.*' "$log" | head -1)
  say "$report"
  # A session to ask with: `ls_skill` is always asked by one.
  ask '{"name":"new_session","args":{"id":"main"}}' >/dev/null 2>&1
}
stop() {
  "$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port" '{"name":"cancel","args":{}}' >/dev/null 2>&1
  for _ in $(seq 1 100); do alive "$server_pid" || break; sleep 0.2; done
  for pid in $server_pid $sup $(serving_home); do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  for _ in $(seq 1 50); do
    [[ -z "$(serving_home)" ]] && break
    sleep 0.2
  done
  server_pid=""; sup=""
}
ask() { "$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port" "$1" 2>&1; }

mkdir -p "$home"

say "== a fresh home reads the checkout, and copies nothing out of it"
start
[[ -d "$home/ship/.git" ]]
ok "the content is a checkout in the home" $?
[[ ! -e "$home/config.toml" && ! -e "$home/skills" && ! -e "$home/lib" ]]
ok "nothing is installed beside it: no config, no skills, no lib" $?
diff -q "$content/config/config.toml" "$home/ship/config/config.toml" >/dev/null
ok "the config this home reads is the content's own file" $?
listed=$(ask '{"name":"ls_skill","args":{"session":"main"}}')
{ [[ "$listed" == *alpha* ]] && [[ "$listed" == *"v1 handoff"* ]]; } && rc=0 || rc=1
ok "ls_skill reads the checkout's skills" $rc
say "$listed"
stop

say "== an edit in the checkout is read by the next call, with no restart"
start
sed -i 's/^description: .*/description: edited in the checkout/' "$home/ship/skills/alpha/SKILL.md"
listed=$(ask '{"name":"ls_skill","args":{"session":"main"}}')
[[ "$listed" == *"edited in the checkout"* ]]
ok "a skill edited under a running server is the skill it lists" $?
say "$listed"
# The config is the checkout's own file, and it is read again on the next call:
# a broken one is named in the log, and the server keeps the last good read.
cp "$home/ship/config/config.toml" "$work/config.keep"
printf '
[model]
duplicate = "table"
' >> "$home/ship/config/config.toml"
ask '{"name":"ls_skill","args":{"session":"main"}}' >/dev/null
grep -q "config.toml" "$log" && grep -q "did not parse" "$log"
ok "the config is read again on the next call, and a broken one is named" $?
say "$(grep 'did not parse' "$log" | head -1)"
cp "$work/config.keep" "$home/ship/config/config.toml"
stop

say "== an older build's copies go, and a file that is not a copy stays"
mkdir -p "$home/.ship-base/home" "$home/skills/alpha" "$home/skills/beta" "$home/lib"
cp "$home/ship/config/config.toml" "$home/config.toml"
cp "$home/ship/config/config.toml" "$home/.ship-base/home/config.toml"
echo 'deadbeef' > "$home/.ship-pin"
cp "$home/ship/skills/alpha/SKILL.md" "$home/skills/alpha/SKILL.md"
cp "$home/ship/skills/beta/SKILL.md" "$home/skills/beta/SKILL.md"
printf 'edited by a person\n' >> "$home/skills/beta/SKILL.md"
cp "$home/ship/python/gongga_skill.py" "$home/lib/gongga_skill.py"
start
[[ ! -e "$home/config.toml" ]]
ok "a config that was the content's own copy is gone" $?
[[ ! -e "$home/.ship-base" && ! -e "$home/.ship-pin" ]]
ok "the merge base and the pin an older build kept are gone" $?
[[ ! -e "$home/skills/alpha" ]]
ok "a skill directory that was a copy of the content's is gone" $?
[[ ! -e "$home/lib/gongga_skill.py" ]]
ok "a Python file that was a copy of the content's is gone" $?
[[ -f "$home/skills/beta/SKILL.md" ]]
ok "a skill directory somebody edited is kept" $?
grep -q 'differs from the content' "$log"
ok "and the log says which one was kept, and why" $?
say "removals: $(grep -c 'is gone' "$log")"
stop

say "== a start with nothing to clean removes nothing"
start
if grep -q 'is gone' "$log"; then rc=1; else rc=0; fi
ok "no file of the home's is taken away a second time" $rc
say "second start: $(grep 'is gone' "$log" | head -2)"
[[ "$(git -C "$home/ship" rev-parse HEAD)" == "$(git -C "$content" rev-parse HEAD)" ]]
ok "and the checkout still stands on the commit the content names" $?
stop

say "== a checkout with no config refuses to start, and names the file"
rm "$home/ship/config/config.toml"
: > "$log"
GONGGA_SHIP_URL="$content" env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  "$server_bin" --home "$home" --bind "127.0.0.1:$(free_port)" --log info >>"$log" 2>&1
code=$?
{ [[ "$code" != 0 ]] && grep -q 'config.toml' "$log"; }
ok "the start is refused (exit $code), and the message names the file" $?
say "$(tail -2 "$log")"

: > "$log"
"$server_bin" --home "$home" --check-config >>"$log" 2>&1
code=$?
{ [[ "$code" == 2 ]] && grep -q 'config.toml' "$log"; }
ok "--check-config refuses the same way (exit $code), without writing" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · content · %s\n' "$sha"
else
  printf 'FAIL · content · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
