#!/usr/bin/env bash
# The binaries a selftest runs, and the refusal that stops a stale build from
# answering as this checkout.
#
# A selftest reads its answers out of `target/debug`, so it believes that executable
# was built from the source beside it. When it was not -- an older build still on
# disk, another worktree's target, a `CARGO_TARGET_DIR` that pointed elsewhere -- the
# run does not fail: it reports the *old* behaviour, and ten honest FAIL lines against
# code that is already fixed read exactly like ten real ones. Measured here: a stale
# server produced eight.
#
# So the executable must exist and be no older than the newest `.rs` under `crates/`.
# The comparison is the filesystem's, not a build stamp's: nothing has to be
# regenerated for it to be right, and it cannot go stale the way a stamp can.
#
#   source "$(dirname "${BASH_SOURCE[0]}")/selftest_bin.sh"
#   bin=$(selftest_bin "$root") || exit 1
#
# `GONGGA_BIN` names another directory of binaries. It does not excuse a stale one --
# a stale binary is the whole failure this refuses.
selftest_bin() { # root -> the directory on stdout, or a refusal on stderr and 1
  local root=$1 dir=${GONGGA_BIN:-$1/target/debug} exe newer
  exe="$dir/gongga-server.exe"
  [[ -x "$exe" ]] || exe="$dir/gongga-server"
  if [[ ! -x "$exe" ]]; then
    printf 'build first: cargo build -p gongga-server\n' >&2
    printf '  no executable under %s\n' "$dir" >&2
    return 1
  fi
  # Any one newer file is enough to refuse, so this stops at the first: naming it is
  # what tells a reader this is staleness rather than a missing build.
  # Test sources cannot change the binary, and another session's uncommitted test edit
# must not refuse a run: only a source file that links into it is evidence.
newer=$(find "$root/crates" \
  \( -name '*.rs' ! -name 'tests.rs' ! -name '*_test.rs' ! -path '*/tests/*' \) \
  -newer "$exe" -print -quit 2>/dev/null)
  if [[ -n "$newer" ]]; then
    printf 'build first: cargo build -p gongga-server\n' >&2
    printf '  %s is older than %s\n' "$exe" "$newer" >&2
    return 1
  fi
  printf '%s\n' "$dir"
}
