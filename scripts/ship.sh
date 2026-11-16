#!/usr/bin/env bash
# Publish the shipped content: personal commits rebased onto the official branch.
#
#   scripts/ship.sh -m "handoff: name the mentor in the guide"   # commit, rebase, push
#   scripts/ship.sh --check                                   # would this build read it?
#   scripts/ship.sh --dry-run -m "..."                        # what would happen
#   scripts/ship.sh --check --local                           # the same, minus the remote
#
# This is `scripts/publish.sh content ...`: one implementation of commit, rebase and
# push, shared with the community roster, and one place that knows where a checkout is
# and which two branches it publishes between. What stays here is the content's own
# half: its flags, and the exit codes a caller depends on.
#
#   0  the content speaks this build's protocol, and is published
#   1  it does not, or the checkout is not on the official or the personal branch, or
#      the push failed: a real disagreement, refuse it
#   2  usage
#   3  the remote could not be reached, so whether that commit is published is still
#      the open question. Nothing else disagreed.
#
# Nothing is written into this repository. A server asks the content's `main` for the
# newest commit that speaks the protocol it was built to read
# (`crates/server/ship.toml`), and the content declares that number in its own
# `ship.toml`. So a shipped prose fix is one push, and a rebuild happens only when
# this build's protocol changes.
#
# The schemas are the publisher's: a reader of a skill reads the file beside its
# package and trusts it, so a signature that moved without its file regenerated is a
# completion that lies. Publishing generates them; `--check` and `--dry-run` write
# nothing and ask whether each file still says what its package does.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# `--help` is this file's own: publish.sh describes repositories, and this one is
# always the same repository.
case "${1:-}" in
  -h | --help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; exit 0 ;;
esac

writing=yes
for argument in "$@"; do
  case "$argument" in
    --check | --dry-run) writing=no ;;
  esac
done
schemas="$here/../ship/python/schemas.py"
if [[ -f "$schemas" ]]; then
  if [[ "$writing" == yes ]]; then
    python "$schemas" "$here/../ship/skills"
  else
    python "$schemas" --check "$here/../ship/skills"
  fi
fi

exec bash "$here/publish.sh" content "$@"
