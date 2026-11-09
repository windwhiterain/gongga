#!/usr/bin/env bash
# What `ship::install` does to a home whose recorded pin has fallen behind it.
#
# A root keeps the pin it was last reconciled against as the merge base, and holds
# that pin still whenever a file conflicts -- deliberately: advancing past an
# unresolved file would adopt the unmerged result as truth. The cost is a home
# whose base is *older than the files a newer pin left*: every file that pin
# touched then looks like "both sides moved", so it conflicts, so the pin never
# advances, so the next start conflicts again. This check pins both ends of that:
# an install of another pin is replaced and the pin moves on, and a file the
# reader really wrote still stops it.
#
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
[[ -d "$root/ship/.git" || -f "$root/ship/.git" ]] || {
  echo "no content repository at $root/ship: git submodule update --init" >&2; exit 1; }

# The content this run compares against: the submodule in a checkout, or wherever
# `GONGGA_SHIP_CONTENT` points -- a linked worktree has no submodule of its own, and
# the commits asked for here are the same commits in either place.
content="${GONGGA_SHIP_CONTENT:-$root/ship}"
# The pin is not carried by this build any more: the server walks the content's
# `main` back, first parents only, to the newest commit that speaks the protocol
# it reads (`ship::newest_for`), and installs that one. A home would install it,
# so that is the pin this test reconciles against -- and a build whose protocol
# has no content yet says so before a home is made.
protocol=$(awk -F' = ' '$1 == "protocol" { print $2 }' "$root/crates/server/ship.toml" | sed 's/[[:space:]]*$//')
[[ -n "$protocol" ]] || { echo "no protocol in $root/crates/server/ship.toml" >&2; exit 1; }
speaks() { git -C "$content" show "$1:ship.toml" 2>/dev/null | awk -F' = ' '$1 == "protocol" { print $2 }' | sed 's/[[:space:]]*$//'; }
pin=""
for commit in $(git -C "$content" rev-list --first-parent HEAD); do
  [[ "$(speaks "$commit")" == "$protocol" ]] && { pin=$commit; break; }
done
[[ -n "$pin" ]] || {
  echo "$content has no commit speaking protocol $protocol: publish content for this build" >&2
  exit 1; }

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
  server_pid=$(child_pid); sup=$(parent_of "$server_pid")
  report=$(grep -o 'shipped content.*' "$log" | head -1)
  say "$report"
}
stop() {
  "$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port" cancel >/dev/null 2>&1
  for _ in $(seq 1 100); do alive "$sup$server_pid" || break; sleep 0.2; done
  for pid in $server_pid $sup; do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  server_pid=""; sup=""
}

# The three versions a stale base produces, derived rather than hardcoded: the
# commits that touched a path, newest first, are the pin's content (`theirs`), the
# version the base pin holds (`base`), and one install older than that (`ours`).
commits_of() { git -C "$content" rev-list "$pin" -- "$1"; }
seed() { # path, mode: stale | reader
  local path=$1 mode=$2 mine base theirs
  mine=$(commits_of "$path" | tail -1)
  base=$(commits_of "$path" | sed -n 2p)
  [[ -n "$mine" && -n "$base" ]] || { echo "no history to seed $path from" >&2; exit 1; }
  mkdir -p "$home/$(dirname "$path")"
  # `base` is the pin this home now claims to have reconciled; the file on disk is
  # what an even older pin left there.
  echo "$base" > "$home/.ship-pin"
  if [[ "$mode" == reader ]]; then
    # The reader's own words on the very line upstream moved: the bytes belong to
    # no commit, so nothing may decide they are disposable.
    git -C "$content" show "$base:$path" | sed 's/^description:.*/description: 读者自己写的一句话。/' > "$home/$path"
  else
    git -C "$content" show "$mine:$path" > "$home/$path"
  fi
}

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
rm -rf "$home"; mkdir -p "$home"
# The home clones the content this run measures, so the commit the check expects
# and the commit the home installs come from one history -- which is the whole
# scenario, and what two different checkouts cannot promise. The URL is a path, so
# the clone is local and no start reaches the network.

say "== an install of another pin is not an edit"
seed skills/heir/SKILL.md stale
seed skills/remote/SKILL.md stale
start
echo "$report" | grep -q 'conflicts=\[\]'; ok "no conflict, though both sides moved" $?
echo "$report" | grep -q 'rebased=\['; ok "the replaced files are named" $?
[[ "$(cat "$home/.ship-pin")" == "$pin" ]]; ok "the pin moves past them" $?
git -C "$content" show "$pin:skills/heir/SKILL.md" | diff -q - "$home/skills/heir/SKILL.md" >/dev/null
ok "and the file is the pinned content, not a merge of two pins" $?
stop

say "== a settled home is not rewritten"
start
saw=$(echo "$report" | head -c 120)
echo "$report" | grep -Eq 'written=0 updated=0 merged=0 removed=0 rebased=\[\] conflicts=\[\]'
ok "a second start changes nothing (saw: $saw)" $?
[[ "$(cat "$home/.ship-pin")" == "$pin" ]]; ok "and leaves the pin where it was" $?
stop

say "== a file the reader wrote still stops the pin"
seed skills/heir/SKILL.md reader
stopped=$(cat "$home/.ship-pin")
start
echo "$report" | grep -q 'conflicts=\[[^]]*heir/SKILL.md'
ok "the reader's edit is a conflict, not an install of another pin" $?
echo "$report" | grep -q 'rebased=\[\]'; ok "and nothing claims it was rebased" $?
[[ "$(cat "$home/.ship-pin")" == "$stopped" ]]; ok "the pin stays where it was" $?
grep -q '读者自己写的一句话' "$home/skills/heir/SKILL.md"; ok "the reader's line is still there" $?
stop

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · install · %s\n' "$sha"
else
  printf 'FAIL · install · %d scenario(s) · %s\n' "$fails" "$sha"; exit 1
fi
