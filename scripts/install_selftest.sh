#!/usr/bin/env bash
# What `ship::install` does to a home as the content moves ahead of it.
#
# The merge base is the bytes this home last installed (`.ship-base`), not a commit
# id: a pin is gone from this build, and the base is what an install wrote. So a
# home that advances past a version whose files the next version also touched has to
# update those files, not call them edited -- anything else leaves a home that can
# never advance again. This check builds a content repository of its own with a real
# history, moves that history forward, and pins both ends: advancing replaces what
# the last install wrote, and a file the reader really wrote still stops the walk.
#
# `GONGGA_SHIP_CONTENT` names a checkout to take the home's `config/config.toml`
# from (a home whose config says nothing about its prompts refuses to start); it
# defaults to this checkout's `ship`, which a linked worktree does not have.
# One line of verdict at the end; `-v` prints what each scenario saw.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
home="$root/.dev/selftest/install-home"
log="$root/.dev/selftest/install.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { # name, condition-result
  if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi
}

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
# The content is this run's own repository: a history the check can move, so every
# install asserted on below is a real install rather than a file written by hand.
protocol=$(awk -F' = ' '$1 == "protocol" { print $2 }' "$root/crates/server/ship.toml" | sed 's/[[:space:]]*$//')
[[ -n "$protocol" ]] || { echo "no protocol in $root/crates/server/ship.toml" >&2; exit 1; }
content="$root/.dev/selftest/install-content"
# The config a home runs on is content, and a scratch repository has to carry a real
# one: a home whose config says nothing about its prompts refuses to start.
from=${GONGGA_SHIP_CONTENT:-$root/ship}
[[ -f "$from/config/config.toml" ]] || {
  echo "no content checkout to take a config from: set GONGGA_SHIP_CONTENT" >&2; exit 1; }
version() { # name, description, body
  printf -- '---\nname: %s\ndescription: %s\n---\n\n%s\n' "$1" "$2" "$3"
}
commit() { git -C "$content" add -A && git -C "$content" commit -qm "$1" && git -C "$content" rev-parse HEAD; }
seed_content() {
  rm -rf "$content"; mkdir -p "$content/skills/alpha" "$content/skills/beta"
  git -C "$content" init -q
  # A branch, and the name a content repository is read by: a detached head gives
  # the fork nothing to take content from.
  git -C "$content" branch -m main
  git -C "$content" config user.email selftest@example.invalid
  git -C "$content" config user.name selftest
  mkdir -p "$content/config"
  # A manifest as the real content writes one: the protocol it speaks, which file
  # is the home's config, and where the skills are.
  cat > "$content/ship.toml" <<TOML
protocol = $protocol

config = "config/config.toml"

[skills]
harness = "skills"
TOML
  cp "$from/config/config.toml" "$content/config/config.toml"
  for n in 1 2 3 4; do
    version alpha "v$n handoff" "body $n" > "$content/skills/alpha/SKILL.md"
    version beta "v$n remote" "body $n" > "$content/skills/beta/SKILL.md"
    eval "c$n=$(commit 'content v$n')"
  done
  # The run begins where a home would: on the first version, with the rest ahead.
  git -C "$content" reset --hard -q "$c1"
}
seed_content

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" 2>/dev/null | tr -d '\r\n'
}
# Every process serving THIS home: identity is the home in the command line, never
# the image name -- another agent's server has the same name as this one.
serving_home() {
  local win posix
  win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}

server_pid=""
sup=""
cleanup() {
  for pid in $(serving_home) "$server_pid" "$sup"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

# One server per scenario: what an install decides is decided at startup, and the
# decision depends on the state seeded just before it. `stop` leaves nothing.
start() {
  : > "$log"
  port=$(free_port)
  GONGGA_SHIP_URL="$content" env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
  for _ in $(seq 1 150); do grep -q "shipped content" "$log" && break; sleep 0.2; done
  # A start that never reached its lock says why, rather than leaving the checks to
  # fail with nothing to read.
  if [[ ! -s "$home/server.lock" ]]; then
    cp "$log" "$log.failed"
    echo "serving this home: $(serving_home)" >&2
  fi
  server_pid=$(child_pid); sup=$(parent_of "$server_pid")
  report=$(grep -o 'shipped content.*' "$log" | head -1)
  say "$report"
}
# One server per scenario, so a stop has to leave none: a home another server still
# holds refuses to start, and the install it owed lands a scenario later.
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

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
rm -rf "$home"; mkdir -p "$home"

say "== an install of another version replaces what it wrote"
start
grep -q 'v1 handoff' "$home/skills/alpha/SKILL.md"
ok "the first install is the version it was pointed at" $?
stop
git -C "$content" reset --hard -q "$c3"
start
echo "$report" | grep -q 'conflicts=\[\]'
ok "no conflict, though the same file moved twice" $?
git -C "$content" show "$c3:skills/alpha/SKILL.md" | diff -q - "$home/skills/alpha/SKILL.md" >/dev/null
ok "the file is the newest content, not a merge of two versions" $?
git -C "$content" show "$c3:skills/beta/SKILL.md" | diff -q - "$home/skills/beta/SKILL.md" >/dev/null
ok "and so is the one beside it" $?
stop

say "== a settled home is not rewritten"
start
saw=$(echo "$report" | head -c 120)
echo "$report" | grep -Eq 'written=0 updated=0 merged=0 removed=0'
ok "a second start changes nothing (saw: $saw)" $?
stop

say "== a file the reader wrote still stops the update"
sed -i 's/^description: .*/description: 读者自己写的一句话。/' "$home/skills/alpha/SKILL.md"
git -C "$content" reset --hard -q "$c4"
start
echo "$report" | grep -q 'conflicts=\[.*alpha/SKILL.md'
ok "the reader's edit is a conflict" $?
grep -q '读者自己写的一句话' "$home/skills/alpha/SKILL.md"
ok "the reader's line is still there" $?
stop

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · install · %s\n' "$sha"
else
  printf 'FAIL · install · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
