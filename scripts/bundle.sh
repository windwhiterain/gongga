#!/usr/bin/env bash
# One file that carries the programs.
#
#   scripts/bundle.sh                 # debug, into .dev/dist/gongga
#   scripts/bundle.sh --release       # a release build
#   scripts/bundle.sh --out /tmp/gongga
#
# There is nothing to append any more: the `payload` feature makes `gongga-bundle`
# build the programs and embed them, so the file cargo produces *is* the
# distribution. That is what makes `cargo install --features payload` enough to
# ship one, with no script and no post-processing step.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

profile=debug
flag=""
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) profile=release; flag=--release; shift ;;
    --out) out=${2:-}; shift 2 ;;
    -h | --help) sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

say() { printf '  %s\n' "$*"; }
die() { echo "bundle: $*" >&2; exit 1; }

say "== build =="
# `cargo bundle` is the alias in `.cargo/config.toml`: the same command, spelled
# once for everybody. `--release` is not part of it.
cargo bundle $flag 2>&1 | tail -n 4

ext=""
[[ "$OSTYPE" == msys* || "$OSTYPE" == cygwin* ]] && ext=".exe"
# The bin is `gongga` (see crates/bundle/Cargo.toml), not the package name: a
# script that copied the old spelling shipped whatever build was lying there.
built="$root/target/$profile/gongga$ext"
[[ -f "$built" ]] || die "$built is missing"

out=${out:-$root/.dev/dist/gongga}
mkdir -p "$(dirname "$out")"
cp "$built" "$out"
chmod +x "$out"
say "$out  $(du -h "$out" | cut -f1)  (the head and the programs it carries, one file)"

say "== what it carries =="
# A scratch home: a build must never place programs in the operator's own
# install, and "does it run" is the same question anywhere.
smoke="$root/.dev/dist/smoke"
rm -rf "$smoke"
for run in 1 2; do
  GONGGA_HOME="$smoke" "$out" --version 2>&1 | tail -n 1 || die "run $run of the bundle failed"
done
rm -rf "$smoke"
say "next: copy it anywhere and run it; it places the programs in \$GONGGA_HOME/bin"
