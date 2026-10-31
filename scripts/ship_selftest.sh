#!/usr/bin/env bash
# Test scripts/ship.sh without touching GitHub: a scratch bare repository is the
# remote, and a local clone of this repo is the working tree.
set -uo pipefail

# Resolved before the cd below, so it is absolute by the time anything uses it.
src=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && git rev-parse --show-toplevel)
base=$(mktemp -d "${TEMPDIR:-/tmp}/ship-test-XXXX")
cd "$base" || exit 1
[[ -d "$src" ]] || { echo "no repository at $src" >&2; exit 1; }

say() { printf '%s\n' "$*"; }
fails=0
check() { # name, ok(0/1 via test), detail
  if eval "$2"; then say "  ok   $1"; else say "  FAIL $1"; fails=$((fails + 1)); fi
}

say "== seed a scratch remote with what ship is today"
git clone -q --bare "$src/ship" shipremote.git
git clone -q "$src" wt || { echo "cloning $src failed" >&2; exit 1; }
[[ -d wt/.git ]] || { echo "no clone at $(pwd)/wt" >&2; exit 1; }
git -C wt config submodule.ship.url "$base/shipremote.git"
git -C wt -c protocol.file.allow=always submodule update --init -q >/dev/null 2>&1
# a clone of a *dirty* repo lacks the untracked script under test; copy the working
# copy in, or the test would only ever check what was already committed.
cp "$src/scripts/ship.sh" wt/scripts/ship.sh
say "   submodule at: $(git -C wt/ship rev-parse --short HEAD) (detached? $(git -C wt/ship symbolic-ref -q HEAD >/dev/null && echo no || echo yes))"

say "== --check on a fresh clone: the detached HEAD is the pin, so it agrees"
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh --check >fresh.txt 2>&1
check "check passes on an untouched clone (detached HEAD == the pin)" "grep -q 'in agreement' fresh.txt"

say "== dry run with changes: nothing must be written"
printf '\nA line added by the release-script test.\n' >> wt/ship/skills/harness/heir/SKILL.md
before=$(git -C wt/ship rev-parse HEAD)
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh --dry-run -m "test" >dry.txt 2>&1
check "dry-run left HEAD alone" "[[ \$(git -C wt/ship rev-parse HEAD) == $before ]]"
check "dry-run says what it would do" "grep -q 'would commit those changes' dry.txt"
sed -i '/A line added by the release-script test./d' wt/ship/skills/harness/heir/SKILL.md

say "== the guard: changes without a message"
printf '\nAnother test line.\n' >> wt/ship/skills/harness/heir/SKILL.md
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh >noguard.txt 2>&1
code=$?
check "refuses to publish a message it invented (exit 2, saw: $(head -1 noguard.txt))" "[[ $code == 2 ]]"
sed -i '/Another test line./d' wt/ship/skills/harness/heir/SKILL.md

say "== the real run, against the scratch remote"
printf '\nPublished by the release-script test.\n' >> wt/ship/skills/harness/heir/SKILL.md
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh -m "test: the release script publishes itself" >run.txt 2>&1
sed -n 's/^/   /p' run.txt
sha=$(git -C wt/ship rev-parse HEAD)
remote_sha=$(git --git-dir="$base/shipremote.git" rev-parse refs/heads/main)
pinned=$(sed -n 's/^commit = "\([0-9a-f]*\)"/\1/p' wt/crates/server/ship.toml)
check "the submodule committed and moved" "[[ $sha != $before ]]"
check "the remote now advertises that commit" "[[ $remote_sha == $sha ]]"
check "ship.toml pins the same sha" "[[ $pinned == $sha ]]"
check "the pin and the submodule are staged here" "git -C wt diff --cached --name-only | grep -q crates/server/ship.toml"
check "the published prose reached the remote" "git --git-dir=$base/shipremote.git cat-file blob $remote_sha:skills/harness/heir/SKILL.md | grep -q 'Published by the release-script test'"

say "== --check now agrees on the release; and it fails when somebody drifts"
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh --check >check.txt 2>&1
check "check says in agreement" "grep -q 'in agreement' check.txt"
sed -i 's/^commit = "[0-9a-f]*"/commit = "0000000000000000000000000000000000000000"/' wt/crates/server/ship.toml
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh --check >drift.txt 2>&1
check "a drifted manifest is reported" "grep -q 'MISMATCH' drift.txt"

say "== and the same drift check catches a pin the remote does not have"
git -C wt checkout -q -- crates/server/ship.toml
git -C wt/ship commit -q --allow-empty -m "a commit nobody published"
GONGGA_SHIP_REMOTE="$base/shipremote.git" bash wt/scripts/ship.sh --check >orphan.txt 2>&1
check "an unpublished pin is refused" "grep -q 'NOT PUBLISHED' orphan.txt"

cd / && rm -rf "$base" wt 2>/dev/null
say "FAILURES: $fails"
[[ $fails == 0 ]]
