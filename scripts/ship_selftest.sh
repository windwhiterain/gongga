#!/usr/bin/env bash
# The content protocol, on real repositories and a real server: what a machine
# installs is the newest commit of the branch that speaks the protocol its build
# reads, and publishing content needs no commit in this repository.
#
# Two halves, because there are two things that can lie. The first drives
# `scripts/ship.sh` against scratch repositories: publish, refuse a protocol drift,
# and tell an unreachable remote apart from a disagreement. The second starts the
# server against a scratch home whose `GONGGA_SHIP_URL` is a scratch repository, and
# reads what actually landed on disk -- the one claim no unit of this repository can
# make, because resolution is a fetch and a walk over real commits.
#
# Each scenario starts its own server: "the newest commit changed" is a fact about a
# start, so a shared lifetime would be testing the previous answer.
#
# One line of verdict at the end.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/ship-selftest"
fails=0
started=$(date +%s)

say() { printf '  %s\n' "$*"; }
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server" >&2; exit 1; }
server_bin="$bin/gongga-server.exe"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server"

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}

# Every pid this script started, by the home it serves -- never by image name.
pids=""
cleanup() {
  local pid
  for pid in $pids; do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 1
  rm -rf "$work" || echo "note: $work is still held by a process" >&2
}
trap cleanup EXIT

rm -rf "$work"; mkdir -p "$work"

# ---------------------------------------------------------------- the content ---
# One git repository, walked forward the way a real one is: three commits, and the
# last one speaks a protocol no build here reads.
seed_content() {
  local repo=$1
  mkdir -p "$repo/config" "$repo/skills/marker"
  cp "$root/ship/config/config.toml" "$repo/config/config.toml"
  git -C "$repo" init -q -b main
  git -C "$repo" config user.email test@example.invalid
  git -C "$repo" config user.name test
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
description: what the last installed commit said
---

$text
SKILL
  git -C "$repo" add -A
  git -C "$repo" commit -q -m "$message"
  git -C "$repo" rev-parse HEAD
}

content=$work/content
seed_content "$content"
commit_a=$(content_commit "$content" 1 "marker A" "protocol 1: A")
commit_b=$(content_commit "$content" 1 "marker B" "protocol 1: B")
commit_c=$(content_commit "$content" 2 "marker C" "protocol 2: C")

remote=$work/remote.git
git clone -q --bare "$content" "$remote" 2>/dev/null
git -C "$content" remote add origin "$remote"
git -C "$content" push -q origin main

# --------------------------------------------------------------- the install ---
start_server() { # home, url, logfile -> prints the pid
  local home=$1 url=$2 log=$3 port pid before
  before=$(tr -d '\r\n' < "$home/server.lock" 2>/dev/null)
  port=$(free_port)
  GONGGA_SHIP_URL="$url" "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info \
    >>"$log" 2>&1 &
  for _ in $(seq 1 200); do
    pid=$(tr -d '\r\n' < "$home/server.lock" 2>/dev/null)
    [[ -n "$pid" && "$pid" != "$before" ]] && break
    sleep 0.2
  done
  pids="$pids $pid"
  printf '%s' "$pid"
}

stop_server() { # pid
  local pid=$1
  [[ -z "$pid" ]] && return 0
  alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  for _ in $(seq 1 50); do
    alive "$pid" || return 0
    sleep 0.2
  done
  say "still alive after the kill: $pid"
  return 1
}

# What the marker skill says on disk, which is the only thing that proves an install.
marker() { sed -n 's/^marker \(.*\)$/\1/p' "$1/skills/marker/SKILL.md" 2>/dev/null | head -1; }
installed_commit() { sed -n 's/^\([0-9a-f]\{40\}\)$/\1/p' "$1/.ship-pin" 2>/dev/null | head -1; }

say "== a branch whose newest commit speaks a newer protocol installs the last one this build reads"
home=$work/home-1; mkdir -p "$home"; log=$work/home-1.log
pid=$(start_server "$home" "$remote" "$log")
sleep 2
say "     log: $(grep -m1 'shipped content' "$log" | sed 's/.*shipped content //')"
m=$(marker "$home"); [[ "$m" == B ]]; rc=$?
ok "protocol 2 at the tip, protocol 1 installed (marker $m)" $rc
got=$(installed_commit "$home"); [[ "$got" == "$commit_b" ]]; rc=$?
ok "the home pins the last protocol-1 commit, not the tip (${got:0:7})" $rc
grep -q "pin=${commit_b:0:10}" "$log"; rc=$?
ok "the log names that commit" $rc
stop_server "$pid"

say "== a content change reaches the same home on the next start, with no rebuild"
commit_d=$(content_commit "$content" 1 "marker D" "protocol 1: D")
git -C "$content" push -q origin main
log=$work/home-2.log
pid=$(start_server "$home" "$remote" "$log")
sleep 2
m=$(marker "$home"); [[ "$m" == D ]]; rc=$?
ok "the next start installed D (marker $m)" $rc
got=$(installed_commit "$home"); [[ "$got" == "$commit_d" ]]; rc=$?
ok "and pinned D (${got:0:7})" $rc
stop_server "$pid"

say "== a branch with no commit this build can read refuses, and says which protocols it found"
fresh=$work/content-2; mkdir -p "$fresh"
seed_content "$fresh"
content_commit "$fresh" 7 "marker seven" "protocol 7 only" >/dev/null
remote7=$work/remote7.git
git clone -q --bare "$fresh" "$remote7" 2>/dev/null
home7=$work/home-7; mkdir -p "$home7"; log7=$work/home-7.log
pid=$(start_server "$home7" "$remote7" "$log7")
sleep 2
grep -q 'no commit on main speaks protocol 1' "$log7"; rc=$?
ok "the server refuses to start and names the protocol" $rc
grep -q 'the newest one speaks 7' "$log7"; rc=$?
ok "and names what the content does speak" $rc
stop_server "$pid"

# ------------------------------------------------------------------ the tool ---
# `scripts/ship.sh` reads the manifest, the content manifest and the remote. A copy
# of just those three paths is enough, and it cannot touch this checkout.
say "== scripts/ship.sh: publish, and tell a drift from a silence"
tool=$work/tool
mkdir -p "$tool/scripts" "$tool/crates/server"
cp "$root/scripts/ship.sh" "$tool/scripts/ship.sh"
cp "$root/crates/server/ship.toml" "$tool/crates/server/ship.toml"
git clone -q "$remote" "$tool/ship" 2>/dev/null
git -C "$tool/ship" checkout -q main
git -C "$tool/ship" config user.email test@example.invalid
git -C "$tool/ship" config user.name test
ship() { bash "$tool/scripts/ship.sh" "$@"; }

ship --check >"$work/check-agree.txt" 2>&1; rc=$?
ok "--check agrees on published, matching content" $rc

ship --push-main >"$work/check-pushmain.txt" 2>&1; rc=$?
[[ $rc == 2 ]]; rc=$?
ok "--push-main is refused as usage, not silently accepted" $rc
sed -i 's/^protocol = 1$/protocol = 2/' "$tool/ship/ship.toml"
ship --check >"$work/check-drift.txt" 2>&1; rc=$?
[[ $rc == 1 ]]; rc=$?
ok "--check refuses content that speaks a protocol this build cannot read" $rc
grep -q 'speaks protocol 2; this build reads 1' "$work/check-drift.txt"
ok "and says which number is whose" $?

sed -i 's/^protocol = 2$/protocol = 1/' "$tool/ship/ship.toml"
GONGGA_SHIP_REMOTE="$work/no-such-remote.git" ship --check >"$work/check-silent.txt" 2>&1; rc=$?
[[ $rc == 3 ]]; rc=$?
ok "a remote that does not answer is 3, not a disagreement" $rc

ship -m "protocol 1: E from the tool" >"$work/publish.txt" 2>&1; rc=$?
ok "publishing a content change succeeds (exit $rc)" $rc
here=$(git -C "$tool/ship" rev-parse HEAD); there=$(git -C "$remote" rev-parse main)
[[ "$here" == "$there" ]]; rc=$?
ok "the pushed commit is the branch tip" $rc
ship --check >"$work/check-after.txt" 2>&1; rc=$?
ok "--check agrees after the publish" $rc

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
    sed 's/\x1b\[[0-9;]*m//g' "$report" | grep -Ei 'shipped content|warn|error|refus' | tail -6 | sed 's/^/     /'
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
