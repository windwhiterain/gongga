#!/usr/bin/env bash
# The content protocol and the personal-over-official checkout, on real repositories
# and a real server.
#
# Three things can lie here, so there are three halves. `scripts/ship.sh` against
# scratch repositories: publish, refuse a protocol drift, and tell an unreachable
# remote apart from a disagreement. A real server against a scratch home whose
# `GONGGA_SHIP_URL` is a scratch repository: what actually landed on disk is the one
# claim no unit of this repository can make, because resolution is a clone, a rebase
# and a walk over real commits. And the personal branch itself: a commit on `local`
# has to survive an upstream change and still be what the home installs.
#
# Every scenario that starts a server starts its own: "the newest commit changed" is
# a fact about a start, so a shared lifetime would be testing the previous answer.
#
# One line of verdict at the end.
set -uo pipefail

# `--keep` leaves the work directory behind, which is the difference between reading a
# failure and repeating it.
keep=0
[[ "${1:-}" == "--keep" ]] && keep=1

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/ship-selftest"
fails=0
started=$(date +%s)

# The protocol this build reads, and one it does not. Every scenario that says
# "content this build can read" means the first number, and every one that says
# "a protocol no build here does" means the second -- so moving the build to a
# newer protocol does not leave a scenario behind asserting the old one.
proto=$(awk -F' = ' '$1 == "protocol" { print $2 }' "$root/crates/server/ship.toml" | sed 's/[[:space:]]*$//')
[[ -n "$proto" ]] || { echo "no protocol in $root/crates/server/ship.toml" >&2; exit 1; }
drift=$((proto + 1))

say() { printf '  %s\n' "$*"; }
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
server_bin="$bin/gongga-server.exe"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server"
# The content checkout whose shapes the scratch repositories are seeded from: the
# submodule, or wherever `GONGGA_SHIP_CONTENT` points -- a linked worktree has no
# submodule of its own. Only read here, never written.
reference="${GONGGA_SHIP_CONTENT:-$root/ship}"
[[ -d "$reference" ]] || {
  echo "no content at $reference: git submodule update --init ship" >&2; exit 1; }

free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }

# The pid a home's lock names, which is the server the supervisor exec'd -- not the
# pid this script launched.
lock_pid() { [[ -f "$1/server.lock" ]] && tr -d '\r\n' < "$1/server.lock" 2>/dev/null; }

# The TOP of every process tree serving THIS home, whatever the run remembers. A start
# is two `gongga-server.exe`: a supervisor, and the server it spawns and would spawn
# again -- so killing the one the lock names brings the other one back, and the tree is
# the only thing that stops a home. Only the command line says which home a tree holds,
# and only the parent chain says which process is its top.
serving_home() {
  local win posix
  win=$(cygpath -w "$1" 2>/dev/null || echo "$1")
  posix=$(cygpath -m "$1" 2>/dev/null || echo "$1")
  powershell -NoProfile -Command \
    "\$all = Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home $posix*' -or \$_.CommandLine -like '*--home $win*' }
     \$byId = @{}; foreach (\$p in \$all) { \$byId[[int]\$p.ProcessId] = \$p }
     foreach (\$p in \$all) {
       \$top = \$p
       while (\$byId.ContainsKey([int]\$top.ParentProcessId)) { \$top = \$byId[[int]\$top.ParentProcessId] }
       \$top.ProcessId
     }" \
    2>/dev/null | tr -d '\r' | sort -u | tr '\n' ' '
}

# Every pid this script started, by the home it serves -- never by image name.
pids=""
cleanup() {
  local pid
  for pid in $pids; do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 1
  if [[ "$keep" == 1 ]]; then
    echo "note: kept $work (--keep)" >&2
    return 0
  fi
  rm -rf "$work" || echo "note: $work is still held by a process" >&2
}
trap cleanup EXIT

# Anything still serving this work directory is from an interrupted run: its
# databases make the next run's homes start half-migrated, and its files make a
# `rm` fail in the middle. Killed by the home it serves, never by image name.
left=$(serving_home "$work")
if [[ -n "${left// /}" ]]; then
  say "a previous run left $(echo $left) serving $work; stopping it"
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
rm -rf "$work"
if [[ -d "$work" ]]; then
  echo "cannot clear $work: still held by $(serving_home "$work")" >&2
  exit 1
fi
mkdir -p "$work"
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid

# ---------------------------------------------------------------- the content ---
# One repository, walked forward the way a real one is: two commits a build can read,
# then one that speaks a protocol no build here does.
seed_content() {
  local repo=$1
  mkdir -p "$repo/config" "$repo/skills/marker"
  cp "$reference/config/config.toml" "$repo/config/config.toml"
  git -C "$repo" init -q -b main
}

content_commit() { # repo, protocol, marker text, message
  local repo=$1 protocol=$2 text=$3 message=$4
  cat > "$repo/ship.toml" <<TOML
protocol = $protocol
config = "config/config.toml"

[skills]
harness = "skills"
TOML
  cat > "$repo/skills/marker/SKILL.md" <<SKILL
---
name: marker
description: what the installed commit said
---

marker $text
SKILL
  git -C "$repo" add -A
  git -C "$repo" commit -q -m "$message"
  git -C "$repo" rev-parse HEAD
}

content=$work/content
seed_content "$content"
content_commit "$content" "$proto" "A" "protocol $proto: A" >/dev/null
commit_b=$(content_commit "$content" "$proto" "B" "protocol $proto: B")

remote=$work/remote.git
git clone -q --bare "$content" "$remote" 2>/dev/null
git -C "$content" remote add origin "$remote"
git -C "$content" push -q origin main

# --------------------------------------------------------------- the install ---
start_server() { # home, url, logfile -> prints the pid
  local home=$1 url=$2 log=$3 port pid before
  before=$(lock_pid "$home")
  port=$(free_port)
  GONGGA_SHIP_URL="$url" env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info \
    >>"$log" 2>&1 &
  for _ in $(seq 1 200); do
    pid=$(lock_pid "$home")
    [[ -n "$pid" && "$pid" != "$before" ]] && break
    sleep 0.2
  done
  pids="$pids $pid"
  printf '%s' "$pid"
}

stop_server() { # launched pid, home
  local launched=$1 home=$2 pid quiet=0
  while [[ $quiet -lt 3 ]]; do
    for pid in $(serving_home "$home"); do
      taskkill //PID "$pid" //T //F >/dev/null 2>&1
      quiet=0
    done
    sleep 0.5
    [[ -z "$(serving_home "$home")" ]] && quiet=$((quiet + 1))
  done
  # The launched pid is the one thing `$!` would have said, and it is already in the
  # tree above -- this is only here for the case where it never became a server.
  [[ -n "$launched" ]] && alive "$launched" && taskkill //PID "$launched" //T //F >/dev/null 2>&1
  [[ -z "$(serving_home "$home")" ]] && return 0
  say "still serving $home: $(serving_home "$home")"
  return 1
}

# An install finishes some time after the lock appears, so every claim about what
# landed waits for the file rather than for a number of seconds.
wait_marker() { # home, expected -> 0 when the marker settles on it
  local home=$1 want=$2 for_=0
  while [[ $for_ -lt 150 ]]; do
    [[ "$(marker "$home")" == "$want" ]] && return 0
    sleep 0.2
    for_=$((for_ + 1))
  done
  return 1
}

wait_log() { # file, text
  local for_=0
  while [[ $for_ -lt 150 ]]; do
    grep -q -- "$2" "$1" 2>/dev/null && return 0
    sleep 0.2
    for_=$((for_ + 1))
  done
  return 1
}

wait_pin() { # home, previous pin -> 0 once the pin has moved off it
  local for_=0
  while [[ $for_ -lt 150 ]]; do
    [[ "$(installed_commit "$1")" != "$2" ]] && return 0
    sleep 0.2
    for_=$((for_ + 1))
  done
  return 1
}

wait_file() { # path
  local for_=0
  while [[ $for_ -lt 150 ]]; do
    [[ -f "$1" ]] && return 0
    sleep 0.2
    for_=$((for_ + 1))
  done
  return 1
}

# What the marker skill says on disk, which is the only thing that proves an install.
marker() { sed -n 's/^marker \(.*\)$/\1/p' "$1/skills/marker/SKILL.md" 2>/dev/null | head -1; }
installed_commit() { sed -n '1p' "$1/.ship-pin" 2>/dev/null | tr -d '\r\n'; }

say "== a fresh home clones the content and installs it"
home=$work/home-1; mkdir -p "$home"; log=$work/home-1.log
pid=$(start_server "$home" "$remote" "$log")
wait_marker "$home" B; rc=$?
ok "the branch's content is installed (marker $(marker "$home"))" $rc
got=$(installed_commit "$home"); [[ "$got" == "$commit_b" ]]; rc=$?
ok "the home pins the commit it installed (${got:0:7})" $rc
[[ -d "$home/ship/.git" ]]; rc=$?
ok "the content is a checkout in the home, not a bare cache" $rc
stop_server "$pid" "$home"

# A personal commit is read where it sits: on top of a commit this build can read.
say "== a personal commit on top is what the home installs, and an upstream change keeps it"
checkout=$home/ship
git -C "$checkout" checkout -q -b local
mkdir -p "$checkout/skills/local-note"
printf -- '---\nname: local-note\ndescription: this machine added it\n---\n\nmine\n' \
  > "$checkout/skills/local-note/SKILL.md"
git -C "$checkout" add -A
git -C "$checkout" commit -q -m "local: a skill only this machine has"
mine=$(git -C "$checkout" rev-parse HEAD)
say "     checkout HEAD: $(git -C "$checkout" rev-parse --abbrev-ref HEAD) $(git -C "$checkout" rev-parse --short HEAD) (personal $(git -C "$checkout" rev-parse --short local 2>/dev/null || echo none))"
log=$work/home-2.log
pid=$(start_server "$home" "$remote" "$log")
wait_file "$home/skills/local-note/SKILL.md"; rc=$?
ok "the personal branch's own skill landed in the home" $rc
stop_server "$pid" "$home"

commit_d=$(content_commit "$content" "$proto" "D" "protocol $proto: D")
git -C "$content" push -q origin main
log=$work/home-3.log
pid=$(start_server "$home" "$remote" "$log")
wait_marker "$home" D; rc=$?
ok "an upstream change arrives (marker $(marker "$home"))" $rc
[[ -f "$home/skills/local-note/SKILL.md" ]]; rc=$?
ok "and the personal skill is still there" $rc
[[ "$(git -C "$checkout" rev-parse --abbrev-ref HEAD)" == local ]]; rc=$?
ok "and the checkout is still on the personal branch" $rc
git -C "$checkout" merge-base --is-ancestor "$commit_d" HEAD; rc=$?
ok "with the personal commit rebased onto the new upstream" $rc
stop_server "$pid" "$home"

say "== content that moves to a newer protocol still installs what this build reads"
commit_c=$(content_commit "$content" "$drift" "C" "protocol $drift: C")
git -C "$content" push -q origin main
was=$(installed_commit "$home")
log=$work/home-4.log
pid=$(start_server "$home" "$remote" "$log")
wait_pin "$home" "$was"; rc=$?
ok "the install moved the home's pin to this content" $rc
got=$(installed_commit "$home")
git -C "$checkout" cat-file -e "$got^{commit}"; rc=$?
ok "and that commit is one the content has (${got:0:7})" $rc
git -C "$checkout" show "$got:ship.toml" | grep -q "^protocol = $proto$"; rc=$?
ok "which speaks this build's protocol" $rc
stop_server "$pid" "$home"

say "== a content with no commit this build can read refuses, and says which protocols it found"
fresh=$work/content-2; mkdir -p "$fresh"
seed_content "$fresh"
content_commit "$fresh" "$drift" "seven" "protocol $drift only" >/dev/null
remote7=$work/remote7.git
git clone -q --bare "$fresh" "$remote7" 2>/dev/null
home7=$work/home-7; mkdir -p "$home7"; log7=$work/home-7.log
pid=$(start_server "$home7" "$remote7" "$log7")
wait_log "$log7" "no commit on main speaks protocol $proto"; rc=$?
ok "the server refuses to start and names the protocol" $rc
wait_log "$log7" "the newest one speaks $drift"; rc=$?
ok "and names what the content does speak" $rc
stop_server "$pid" "$home7"

say "== a home from an older build adopts its bare cache instead of needing the network"
home8=$work/home-8; mkdir -p "$home8"
git clone -q --bare "$remote" "$home8/ship.git" 2>/dev/null
git --git-dir="$home8/ship.git" fetch -q "$remote" '+refs/heads/*:refs/heads/ship/*'
log8=$work/home-8.log
pid=$(start_server "$home8" "$remote" "$log8")
wait_log "$log8" 'the old content cache is now a checkout'; rc=$?
ok "the old cache is adopted" $rc
wait_marker "$home8" D; rc=$?
ok "and the home installs from it (marker $(marker "$home8"))" $rc
stop_server "$pid" "$home8"

# ------------------------------------------------------------------ the tool ---
# `scripts/ship.sh` reads the manifest, the content manifest, the checkout and the
# branch constants. A copy of those is enough, and it cannot touch this checkout.
say "== scripts/ship.sh: publish, and tell a drift from a silence"
tool=$work/tool
mkdir -p "$tool/scripts" "$tool/crates/server/src"
cp "$root/scripts/ship.sh" "$tool/scripts/ship.sh"
cp "$root/crates/server/ship.toml" "$tool/crates/server/ship.toml"
cp "$root/crates/server/src/fork.rs" "$tool/crates/server/src/fork.rs"
# A repository of its own, all of it readable by this build: the tool's question is
# whether this build can read what it is about to publish, not what some other
# scenario left on the branch.
toolcontent=$work/tool-content
seed_content "$toolcontent"
tool_tip=$(content_commit "$toolcontent" "$proto" "T" "protocol $proto: T")
toolremote=$work/tool-remote.git
git clone -q --bare "$toolcontent" "$toolremote" 2>/dev/null
git -C "$toolcontent" remote add origin "$toolremote"
git -C "$toolcontent" push -q origin main
git clone -q "$toolremote" "$tool/ship" 2>/dev/null
git -C "$tool/ship" checkout -q -b local
ship() { bash "$tool/scripts/ship.sh" "$@"; }

ship --check >"$work/check-agree.txt" 2>&1; rc=$?
ok "--check agrees on content this build can read (exit $rc)" $rc

ship --push-main >"$work/check-pushmain.txt" 2>&1; rc=$?
[[ $rc == 2 ]]; rc=$?
ok "--push-main is refused as usage, not silently accepted" $rc

sed -i "s/^protocol = $proto$/protocol = $drift/" "$tool/ship/ship.toml"
ship --check >"$work/check-drift.txt" 2>&1; rc=$?
[[ $rc == 1 ]]; rc=$?
ok "--check refuses content that speaks a protocol this build cannot read" $rc
grep -q "speaks protocol $drift; this build reads $proto" "$work/check-drift.txt"; rc=$?
ok "and says which number is whose" $rc
git -C "$tool/ship" checkout -q -- ship.toml

# The refusal is the tool's own answer, before anything is pushed.
GONGGA_SHIP_REMOTE="$work/no-such-remote.git" ship --check >"$work/check-silent.txt" 2>&1; rc=$?
[[ $rc == 3 ]]; rc=$?
ok "a remote that does not answer is 3, not a disagreement" $rc

ship -m "protocol $proto: this machine's marker" >"$work/publish.txt" 2>&1; rc=$?
ok "publishing content succeeds (exit $rc)" $rc
here=$(git -C "$tool/ship" rev-parse HEAD); there=$(git -C "$toolremote" rev-parse main)
[[ "$here" == "$there" ]]; rc=$?
ok "the published commit is the official tip" $rc

# Publishing is a rebase first: a commit pushed elsewhere has to be walked over, not
# fought with, and the personal commit ends up on top of it.
other=$work/tool-other
git clone -q "$toolremote" "$other" 2>/dev/null
printf 'marker upstream\n' > "$other/skills/marker/SKILL.md"
git -C "$other" commit -q -am "upstream moved the marker"
git -C "$other" push -q origin main
upstream=$(git -C "$toolremote" rev-parse main)
printf 'only this machine\n' > "$tool/ship/personal.md"
ship -m "personal: a file only this machine has" >"$work/publish-2.txt" 2>&1; rc=$?
ok "a personal commit publishes on top of a newer official one (exit $rc)" $rc
git -C "$tool/ship" merge-base --is-ancestor "$upstream" HEAD; rc=$?
ok "and the official commit it walked over is an ancestor of it" $rc
ship --check >"$work/check-after.txt" 2>&1; rc=$?
ok "and the result still reads as this build's protocol (exit $rc)" $rc

# ---------------------------------------------------------------- the verdict ---
if [[ $fails != 0 ]]; then
  for report in "$work"/*.txt; do
    [[ -f "$report" ]] || continue
    say "-- ${report##*/}"
    sed 's/^/     /' "$report" | tail -8
  done
  for report in "$work"/*.log; do
    [[ -f "$report" ]] || continue
    say "-- ${report##*/}"
    sed 's/\x1b\[[0-9;]*m//g' "$report" | grep -Ei 'shipped content|warn|error|refus|adopted' | tail -6 | sed 's/^/     /'
  done
fi
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
elapsed=$(($(date +%s) - started))
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · ship-protocol · %s\n' "$elapsed" "$sha"
else
  printf 'FAIL %s · %ss · ship-protocol · %s\n' "$fails" "$elapsed" "$sha"
fi
exit $((fails > 0))
