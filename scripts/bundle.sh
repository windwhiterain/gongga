#!/usr/bin/env bash
# Build the programs, then write one file that carries all of them.
#
#   scripts/bundle.sh                      # debug, into .dev/dist/
#   scripts/bundle.sh --release            # a release build
#   scripts/bundle.sh --only gongga-cli,gongga-server
#
# Why one file: three programs that only work together are three things to keep
# in step by hand. The head is `gongga-bundle` (a few hundred KB, no
# dependencies); the programs are appended after it, and the table that says
# where is at the very end, so a copy named `gongga` is a command line and one
# named `gongga-server` is a server, without a flag and without a second file.
# The PE loader ignores what comes after the image; on unix nothing looks.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

profile=debug
only="gongga-cli,gongga-server,gongga-tui"
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) profile=release; shift ;;
    --only) only=${2:-}; shift 2 ;;
    --out) out=${2:-}; shift 2 ;;
    -h | --help) sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ "$profile" == release ]] && flag=--release || flag=""

say() { printf '  %s\n' "$*"; }
die() { echo "bundle: $*" >&2; exit 1; }

say "== build =="
# One package at a time: `--workspace` would also relink whatever the operator is
# running, and on Windows that fails with "access denied" instead of a bundle.
for name in ${only//,/ }; do
  cargo build -p "$name" $flag 2>&1 | tail -n 3
done

out=${out:-$root/.dev/dist/gongga}
dir=$(dirname "$out")
mkdir -p "$dir"

say "== write =="
python - "$root" "$profile" "$only" "$out" <<'PY'
import os, struct, sys

root, profile, only, out = sys.argv[1:5]
MAGIC = b"GONGGA-BUNDLE\x00\x00\x00"
EXT = ".exe" if os.name == "nt" else ""

def fnv(data: bytes) -> int:
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h

stub = os.path.join(root, "target", profile, "gongga-bundle" + EXT)
if not os.path.isfile(stub):
    sys.exit(f"bundle: {stub} is missing; run cargo build -p gongga-bundle")

payloads = []
for name in only.split(","):
    path = os.path.join(root, "target", profile, name + EXT)
    if not os.path.isfile(path):
        sys.exit(f"bundle: {path} is missing")
    payloads.append((name, open(path, "rb").read()))

head = open(stub, "rb").read()
entries = b""
offset = len(head)
blob = b""
for name, data in payloads:
    raw = name.encode()
    entries += struct.pack("<H", len(raw)) + raw
    entries += struct.pack("<QQQ", offset, len(data), fnv(data))
    blob += data
    offset += len(data)
table = struct.pack("<I", len(payloads)) + entries
footer = struct.pack("<Q", len(table)) + MAGIC
with open(out, "wb") as f:
    f.write(head + blob + table + footer)
os.chmod(out, 0o755)
print(f"  {out}  {os.path.getsize(out) / 1048576:.1f} MB = "
      f"{len(head) / 1024:.0f} KB head + {', '.join(n for n, _ in payloads)}")
PY

say "== what it carries =="
# A scratch home: building a bundle must never place programs in the operator's
# own install, and "does it run" is the same question anywhere.
smoke="$root/.dev/dist/home"
rm -rf "$smoke"
GONGGA_HOME="$smoke" "$out" --version 2>&1 | tail -n 1 || die "the bundle does not run"
GONGGA_HOME="$smoke" "$out" --version 2>&1 | tail -n 1 || die "the second run does not"
rm -rf "$smoke"
say "next: copy it somewhere and run it; it places the programs in \$GONGGA_HOME/bin"
