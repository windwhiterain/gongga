#!/usr/bin/env bash
# Publish the shipped content, and say whether this build can read it.
#
#   scripts/ship.sh -m "heir: name the mentor in the guide"   # commit + push ship/
#   scripts/ship.sh --check                                   # do the two sides agree?
#   scripts/ship.sh --check --local                           # the same, minus the remote
#   scripts/ship.sh --dry-run -m "..."                        # what would happen
#
# Nothing is written into this repository any more. The pin was two names for one
# commit, so a wording fix in a skill cost an edit here and a rebuild; a server now
# asks the content's branch for the newest commit that speaks the protocol it was
# built to read (`crates/server/ship.toml`). Publishing content is therefore one
# push, and the content declares where its own parts belong (`ship/ship.toml`).
#
# What is left for this script is the one thing a push cannot state: that the
# content on the branch speaks the protocol this build reads. `--check` answers it,
# and answers with four codes, because "they disagree" and "I could not tell" are
# different facts a caller has to be able to act on:
#
#   0  the content speaks this build's protocol, and is published
#   1  it does not -- a real disagreement, refuse it
#   2  usage
#   3  the remote could not be reached, so whether that commit is fetchable is
#      still the open question. Nothing else disagreed.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ship="$root/ship"
manifest="$root/crates/server/ship.toml"
# A second remote is how the script is tested: pushing to a scratch repository
# exercises every step, including the push, without publishing anything real.
remote=${GONGGA_SHIP_REMOTE:-origin}
branch=${GONGGA_SHIP_BRANCH:-main}
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
    --check)   mode=check; shift ;;
    # The half of the question a commit can answer without a network: pre-commit
    # refuses a commit whose content this build cannot read, and says nothing about
    # whether that content is published -- nothing here names a commit any more.
    --local)   local_only=1; shift ;;
    --dry-run) mode=dry; shift ;;
    --push-main)
      # The flag named the half of the old flow that no longer exists: there is no
      # pin here for a main-branch commit to carry, so accepting it would only
      # teach a caller that it still does something.
      echo "--push-main is gone: content is published, not pinned (see the header)" >&2
      exit 2
      ;;
    -h | --help) sed -n '2,32p' "${BASH_SOURCE[0]}"; exit 0 ;;
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

# The field this script reads out of a `ship.toml`: one at the manifest (where the
# server says what it reads) and one in the content (where the content says what it
# is). A missing field is empty, which every comparison below treats as a refusal.
field() { # file, key
  sed -n "s/^$2 = \"\{0,1\}\([^\"[:space:]]*\)\"\{0,1\}$/\1/p" "$1" 2>/dev/null | head -1
}

local_protocol=$(field "$manifest" protocol)
local_branch=$(field "$manifest" branch)
# The file a person just edited answers first: a `--check` before the commit has to
# see what is about to be published, not what was published last.
content_protocol=$(field "$ship/ship.toml" protocol)
if [[ -z "$content_protocol" ]] && git -C "$ship" cat-file -e HEAD:ship.toml 2>/dev/null; then
  content_protocol=$(git -C "$ship" show HEAD:ship.toml | field /dev/stdin protocol || true)
fi
local_sha=$(git -C "$ship" rev-parse HEAD)
dirty=$(git -C "$ship" status --porcelain)
branch_sha=$(git -C "$ship" symbolic-ref -q --short HEAD || true)

say "ship HEAD   $local_sha (${branch_sha:-detached})"
say "protocol    content ${content_protocol:-<none>}, this build ${local_protocol:-<none>}"
say "branch      this build reads ${local_branch:-<none>}, publishing to $branch"
[[ -n "$dirty" ]] && { say "uncommitted in ship/:"; printf '%s\n' "$dirty" | sed 's/^/      /'; }

# Whether the content is published is a question for the remote, and a remote that
# does not answer leaves it open rather than answering no. See the exit codes.
remote_sha=$(git -C "$ship" ls-remote "$remote" "refs/heads/$branch" 2>/dev/null | cut -f1 || true)

verify() { # prints what disagrees; returns 0 agree, 1 disagree, 3 remote silent
  local agree=0 reachable=1
  if [[ -z "$content_protocol" ]]; then
    disagree "MISMATCH the content does not declare a protocol (ship/ship.toml: protocol = <n>)"
    agree=1
  elif [[ "$content_protocol" != "$local_protocol" ]]; then
    disagree "MISMATCH the content speaks protocol $content_protocol; this build reads $local_protocol"
    disagree "         a server would walk back to the newest commit it can read, or refuse to start"
    agree=1
  fi
  if [[ -n "$local_branch" && "$local_branch" != "$branch" ]]; then
    disagree "MISMATCH this build reads branch $local_branch; publishing here goes to $branch"
    agree=1
  fi
  if [[ $local_only == 1 ]]; then
    :
  elif [[ -z "$remote_sha" ]]; then
    disagree "UNVERIFIED $remote did not answer for $branch: cannot tell whether the content is published"
    reachable=0; agree=1
  elif ! git -C "$ship" merge-base --is-ancestor "$local_sha" "$remote_sha" 2>/dev/null; then
    if git -C "$ship" merge-base --is-ancestor "$remote_sha" "$local_sha" 2>/dev/null; then
      disagree "NOT PUBLISHED $remote/$branch is behind ship HEAD: run this without --check"
    else
      disagree "MISMATCH $remote/$branch and ship/ have diverged: this script never force-pushes"
    fi
    agree=1
  fi
  if [[ $agree == 0 ]]; then
    say "in agreement"
    return 0
  fi
  # Unreachable only counts as "cannot tell" when nothing else disagreed.
  if [[ $reachable == 0 && $agree == 1 ]] && protocol_agrees; then
    return 3
  fi
  return 1
}

# The comparison that does not need a network, so an offline clone can still tell a
# real disagreement from an unreachable remote.
protocol_agrees() {
  [[ -n "$content_protocol" && "$content_protocol" == "$local_protocol" ]] &&
    [[ -z "$local_branch" || "$local_branch" == "$branch" ]]
}

if [[ $mode == check ]]; then
  code=0
  verify || code=$?
  exit "$code"
fi

if [[ $mode == dry ]]; then
  if [[ -z "$dirty" ]]; then
    say "nothing to commit in ship/; the push would publish $(git -C "$ship" rev-parse --short HEAD)"
  elif [[ -n "$message" ]]; then
    say "would commit those changes and push $branch to $remote"
  else
    say "(no -m: nothing would be committed)"
  fi
  exit 0
fi

# A submodule is cloned detached at the commit it was pinned to, so "the branch" may
# not exist here yet.
if [[ -z "$branch_sha" ]]; then
  say "ship/ is detached; working on a new $branch"
  git -C "$ship" checkout -B "$branch" >/dev/null
elif [[ "$branch_sha" != "$branch" ]]; then
  echo "  ship/ is on '$branch_sha', not '$branch'; switch first -- this script does not guess which branch is the release" >&2
  exit 1
fi

# Never push over something nobody pulled: a rejected push mid-script leaves the
# commit made and the branch unmoved, which is the drift this script exists to avoid.
if [[ -n "$remote_sha" && "$remote_sha" != "$local_sha" ]] &&
   ! git -C "$ship" merge-base --is-ancestor "$remote_sha" "$(git -C "$ship" rev-parse HEAD)" 2>/dev/null; then
  echo "  $remote/$branch has commits ship/ does not; pull or rebase first (this script never force-pushes)" >&2
  exit 1
fi

if [[ -n "$dirty" ]]; then
  [[ -n "$message" ]] || { echo "  -m is required when ship/ has changes; do not publish a message you did not write" >&2; exit 2; }
  git -C "$ship" add -A
  git -C "$ship" commit -q -m "$message"
  echo "  committed $(git -C "$ship" rev-parse --short HEAD): $(printf '%s' "$message" | head -c 60)"
elif [[ -z "$message" ]]; then
  echo "  nothing to commit in ship/"
fi

local_sha=$(git -C "$ship" rev-parse HEAD)
if [[ "$local_sha" != "${remote_sha:-}" ]]; then
  if ! pushed=$(git -C "$ship" push "$remote" "HEAD:refs/heads/$branch" 2>&1); then
    printf '%s\n' "$pushed" | sed 's/^/    /' >&2
    echo "  the push failed; nothing in this repository names a commit, so a failure here only means the content did not move" >&2
    exit 1
  fi
  echo "  pushed $branch to $remote ($(printf '%s' "$pushed" | tail -1 | tr -s ' '))"
fi

# A publish proves itself: ask the remote again, with what it now says.
remote_sha=$(git -C "$ship" ls-remote "$remote" "refs/heads/$branch" 2>/dev/null | cut -f1 || true)
code=0
verify || code=$?
# A server installs at its start, so the content reaches a running machine through a
# restart -- `evolve` is how one replaces itself. The build only has to be redone when
# this build's own protocol changed, which is a source edit, not a content edit.
echo "  reminder: a running server installs the content at its next start (evolve), not now"
exit "$code"
