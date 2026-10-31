#!/usr/bin/env bash
# Run the heir script against the schema this checkout's server actually writes.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [[ -n ${PYTHON:-} ]]; then
  python=$PYTHON
elif python3 -c 'import sys' >/dev/null 2>&1; then
  python=python3
else
  python=python
fi
tmp="$root/target/heir-selftest"
mkdir -p "$tmp"

TMPDIR="$tmp" TMP="$tmp" TEMP="$tmp" \
  "$python" "$root/ship/skills/harness/heir/test_heir.py" \
  --schema-source "$root/crates/server/src/store.rs"
