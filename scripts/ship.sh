#!/usr/bin/env bash
# Publish the shipped content: personal commits rebased onto the official branch.
#
#   scripts/ship.sh -m "handoff: name the mentor in the guide"   # commit, rebase, push
#   scripts/ship.sh --check                                   # would this build read it?
#   scripts/ship.sh --dry-run -m "..."                        # what would happen
#   scripts/ship.sh --check --local                           # the same, minus the remote
#
# The content is its own repository, kept the way the community roster is
# (`fork`): `main` is official, `local` is this machine's own commits, and a
# refresh is a fast-forward of `main` followed by a rebase of `local` onto it.
# This checkout may stand on either: on `local` a commit is personal and is
# rebased onto `main` before it goes out; on `main` a commit is already where it
# belongs. Both push `HEAD:main`, so the published history stays a straight line,
# and neither ever puts `local` on the remote -- a machine's own branch is not
# something to publish.
#
# Nothing is written into this repository. The pin is gone: a server asks the
# content's `main` for the newest commit that speaks the protocol it was built to
# read (`crates/server/ship.toml`), and the content declares that number in its own
# `ship.toml`. So a shipped prose fix is one push, and a rebuild happens only when
# this build's protocol changes.
#
# `--check` answers the one question a push cannot state: would the content this
# checkout would publish be readable by this build? Four codes, because "it would
# not" and "I could not tell" are different facts a caller has to act on:
#
#   0  the content speaks this build's protocol, and is published
#   1  it does not -- a real disagreement, refuse it
#   2  usage
#   3  the remote could not be reached, so whether that commit is published is
#      still the open question. Nothing else disagreed.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ship="$root/ship"
manifest="$root/crates/server/ship.toml"
source_of_truth="$root/crates/server/src/fork.rs"
# A second remote is how the script is tested: pushing to a scratch repository
# exercises every step, including the push, without publishing anything real.
remote=${GONGGA_SHIP_REMOTE:-origin}
message=""
mode=ship
local_only=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --message)
      message=${2:-}
      [[ -z "$message" ]] && { echo "-m needs a message" >&2; exit 2; }
      shift 2
      ;;
    --check) mode=check; shift ;;
    --dry-run) mode=dry; shift ;;
    # The half of the question a commit answers without a network: pre-commit
    # refuses a commit whose content this build cannot read, and says nothing about
    # whether that content is published.
    --local) local_only=1; shift ;;
    --push-main)
      echo "--push-main is gone: content is published, not pinned (see the header)" >&2
      exit 2
      ;;
    -h | --help) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
  esac
done

[[ -d "$ship/.git" || -f "$ship/.git" ]] || {
  echo "no ship/ here: git submodule update --init" >&2
  exit 1
}
[[ -f "$manifest" ]] || { echo "no $manifest to read" >&2; exit 1; }

say() { printf '  %s\n' "$*"; }
disagree() { printf '  %s\n' "$*" >&2; }

# The branch names are the server's, read from where they are defined rather than
# copied here: two names for one branch is how the pin drifted in the first place.
branch_of() { sed -n "s/^pub const $1: &str = \"\([^\"]*\)\";$/\1/p" "$source_of_truth" | head -1; }
official=$(branch_of OFFICIAL)
personal=$(branch_of PERSONAL)
[[ -n "$official" && -n "$personal" ]] || {
  echo "cannot read OFFICIAL/PERSONAL from $source_of_truth" >&2; exit 2; }

# A field out of a `ship.toml`: one at the manifest (what the server reads) and one
# in the content (what the content is).
field() { sed -n "s/^$2 = \"\{0,1\}\([^\"[:space:]]*\)\"\{0,1\}$/\1/p" "$1" 2>/dev/null | head -1; }

local_protocol=$(field "$manifest" protocol)
# The file a person just edited answers first: a check before the commit has to see
# what is about to be published, not what was published last.
content_protocol=$(field "$ship/ship.toml" protocol)
if [[ -z "$content_protocol" ]] && git -C "$ship" cat-file -e "HEAD:ship.toml" 2>/dev/null; then
  content_protocol=$(git -C "$ship" show HEAD:ship.toml | field /dev/stdin protocol || true)
fi
local_sha=$(git -C "$ship" rev-parse HEAD)
current=$(git -C "$ship" symbolic-ref -q --short HEAD || true)
dirty=$(git -C "$ship" status --porcelain)

say "ship HEAD   $local_sha (${current:-detached})"
say "protocol    content ${content_protocol:-<none>}, this build ${local_protocol:-<none>}"
say "branches    official $official, personal $personal, publishing ${current:-<detached>} -> ${official}"
[[ -n "$dirty" ]] && { say "uncommitted in ship/:"; printf '%s\n' "$dirty" | sed 's/^/      /'; }

# Whether the content is published is a question for the remote, and a remote that
# does not answer leaves it open rather than answering no. See the exit codes.
remote_sha=$(git -C "$ship" ls-remote "$remote" "refs/heads/$official" 2>/dev/null | cut -f1 || true)
official_here=$(git -C "$ship" rev-parse --verify -q "refs/heads/$official" 2>/dev/null || true)

# The comparison that needs no network, so an offline clone still tells a real
# disagreement from an unreachable remote.
protocol_agrees() {
  [[ -n "$content_protocol" && "$content_protocol" == "$local_protocol" ]]
}

verify() { # 0 agree, 1 disagree, 3 remote silent
  local agree=0 reachable=1
  if [[ -z "$content_protocol" ]]; then
    disagree "MISMATCH the content does not declare a protocol (ship/ship.toml: protocol = <n>)"
    agree=1
  elif ! protocol_agrees; then
    disagree "MISMATCH the content speaks protocol $content_protocol; this build reads $local_protocol"
    disagree "         a server walks back to the newest commit it can read, or refuses to start"
    agree=1
  fi
  if [[ $local_only == 1 ]]; then
    [[ $agree == 0 ]] && say "in agreement"
    return $(( agree == 0 ? 0 : 1 ))
  fi
  if [[ -z "$remote_sha" ]]; then
    disagree "UNVERIFIED $remote did not answer for $official: cannot tell whether the content is published"
    reachable=0; agree=1
  else
    # What would be published is this checkout's HEAD, rebased: `official` here is
    # its base, and the remote has to already have that base.
    if [[ -n "$official_here" ]] && ! git -C "$ship" merge-base --is-ancestor "$official_here" "$remote_sha" 2>/dev/null; then
      if git -C "$ship" merge-base --is-ancestor "$remote_sha" "$official_here" 2>/dev/null; then
        disagree "NOT PUBLISHED $remote/$official is behind $official here: run this without --check"
      else
        disagree "MISMATCH $remote/$official and $official here have diverged: pull or rebase first"
      fi
      agree=1
    fi
  fi
  if [[ $agree == 0 ]]; then
    say "in agreement"
    return 0
  fi
  # Unreachable counts as "cannot tell" only when nothing else disagreed.
  if [[ $reachable == 0 ]] && protocol_agrees; then
    return 3
  fi
  return 1
}

if [[ $mode == check ]]; then
  code=0
  verify || code=$?
  exit "$code"
fi

if [[ $mode == dry ]]; then
  if [[ -n "$dirty" && -z "$message" ]]; then
    say "(no -m: nothing would be committed)"
  elif [[ -n "$dirty" ]]; then
    say "would commit those changes on ${current:-$personal}, rebase onto $official, push to $remote/$official"
  else
    say "nothing to commit; the push would publish ${current:-$personal} on top of $official"
  fi
  exit 0
fi

# A submodule is cloned detached at whatever commit it was left on, so the personal
# branch may not be checked out here yet.
if [[ -z "$current" ]]; then
  say "ship/ is detached; working on a new $personal"
  git -C "$ship" checkout -B "$personal" >/dev/null
elif [[ "$current" != "$personal" && "$current" != "$official" ]]; then
  echo "  ship/ is on '$current': publish from '$official' (a commit that belongs to everyone)" >&2
  echo "  or from '$personal' (this machine's own, rebased onto '$official' first); switch first" >&2
  exit 1
fi

if [[ -n "$dirty" ]]; then
  [[ -n "$message" ]] || { echo "  -m is required when ship/ has changes; do not publish a message you did not write" >&2; exit 2; }
  git -C "$ship" add -A
  # Staged is not the same question as changed: `git status` reports a line-ending
  # difference that normalisation then absorbs, and a commit of nothing fails.
  if git -C "$ship" diff --cached --quiet; then
    echo "  nothing to commit in ship/ (the index matches HEAD)"
  else
    git -C "$ship" commit -q -m "$message"
    echo "  committed $(git -C "$ship" rev-parse --short HEAD): $(printf '%s' "$message" | head -c 60)"
  fi
elif [[ -z "$message" ]]; then
  echo "  nothing to commit in ship/"
fi

# The rebase that comes before the push: publishing a personal commit means putting
# it on top of what official is now, never a merge nobody can read later.
remote_sha=$(git -C "$ship" ls-remote "$remote" "refs/heads/$official" 2>/dev/null | cut -f1 || true)
if [[ -n "$remote_sha" ]]; then
  # `ship/.git` may be a file (a submodule), so the git dir is asked for.
  if ! fetched=$(git -C "$ship" fetch "$remote" "+refs/heads/$official:refs/remotes/$remote/$official" 2>&1); then
    printf '%s\n' "$fetched" | sed 's/^/    /' >&2
    echo "  $remote did not answer; nothing was pushed" >&2
    exit 1
  fi
  if ! git -C "$ship" rebase -q "refs/remotes/$remote/$official"; then
    git -C "$ship" rebase --abort >/dev/null 2>&1 || true
    echo "  $personal conflicts with $remote/$official; the rebase was undone, nothing was pushed" >&2
    echo "  resolve it by hand, then run this again" >&2
    exit 1
  fi
  echo "  rebased $current onto $remote/$official ($(git -C "$ship" rev-parse --short "$remote/$official"))"
fi

local_sha=$(git -C "$ship" rev-parse HEAD)
if ! pushed=$(git -C "$ship" push "$remote" "HEAD:refs/heads/$official" 2>&1); then
  printf '%s\n' "$pushed" | sed 's/^/    /' >&2
  echo "  the push failed; the content did not move" >&2
  exit 1
fi
echo "  pushed $official to $remote ($(printf '%s' "$pushed" | tail -1 | tr -s ' '))"
# The local official branch follows what was just published, so the next check and
# the next fetch agree with the remote.
git -C "$ship" fetch -q "$remote" "+refs/heads/$official:refs/heads/$official" 2>/dev/null || true

code=0
verify || code=$?
# A server installs the content at its start, so a running machine sees this after
# `evolve`. The build is redone only when this build's own protocol changed.
echo "  reminder: a running server installs the content at its next start (evolve), not now"
exit "$code"
