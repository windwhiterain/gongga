#!/usr/bin/env bash
# Publish a checkout kept the `fork` way: official commits on one branch, this
# machine's own commits on the other, a rebase before the push.
#
#   scripts/publish.sh <repo> -m "..."         # commit, rebase onto origin/<official>, push
#   scripts/publish.sh <repo> --check          # agreement, and publication
#   scripts/publish.sh <repo> --check --local  # agreement only, no network
#   scripts/publish.sh <repo> --dry-run -m "..."
#   scripts/publish.sh <repo> --where          # the configuration a name resolves to
#   scripts/publish.sh <repo> --state          # what that checkout is, read-only
#
# One implementation, three names. What differs between them -- where the checkout
# is, which remote it publishes to, whether it declares a protocol, whether it may be
# published at all -- is `repo_config`, and no other line here knows a name.
# `scripts/ship.sh` is the content's front end: same flags, same exit codes, and it
# is this file with `content` already chosen, so no caller has to know this one.
#
# Exit codes
#
#   0  agreed, and published
#   1  a real disagreement: protocol drift, a branch that may not publish, a push
#      that failed. Refuse the next step on it.
#   2  usage
#   3  the remote did not answer, so whether it is published is still open. Not a
#      disagreement -- the same call may be fine once the network is back.
#
# `--check` answers two questions together: does what this checkout would publish
# agree with the build that reads it (a protocol, where the repository has one), and
# is that commit published. `--local` asks the first only, which is the half a commit
# is allowed to depend on -- scripts/hooks/pre-commit.
#
# Every git call here names its git dir and its work tree. `git -C` answers for the
# repository *above* a directory whose `.git` git cannot read -- an interrupted clone
# leaves exactly that -- and this script commits, rebases and pushes whatever it was
# pointed at. See AGENTS.md, "主仓上游".
set -euo pipefail

# `git status` refreshes the index unless told not to, which would make `--state` a
# write. A commit's own commands still take their locks; this drops the optional ones.
export GIT_OPTIONAL_LOCKS=0

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$root/crates/server/ship.toml"
source_of_truth="$root/crates/server/src/fork.rs"

repo=""
mode=publish
message=""
local_only=0

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }
say() { printf '  %s\n' "$*"; }
disagree() { printf '  %s\n' "$*" >&2; }

# What differs between the checkouts this flow knows. The source repository is
# described here too, and `publishes=0` is why: one command reads all three, and the
# one thing the source repository must never do is publish -- its `main` is this
# machine's line and a checkpoint merges into it.
repo_config() {
  case "$1" in
    content)
      repo_path=$root/ship
      remote=${GONGGA_SHIP_REMOTE:-origin}
      publishes=1
      check_protocol=1
      has_personal=1
      ;;
    agents)
      repo_path=${GONGGA_AGENTS_PATH:-$HOME/.agents/gongga-agents}
      remote=${GONGGA_AGENTS_REMOTE:-origin}
      publishes=1
      # A roster is prose a server reads, not a shape it parses: there is nothing
      # here for a protocol to disagree about.
      check_protocol=0
      has_personal=1
      ;;
    source)
      repo_path=$root
      remote=origin
      publishes=0
      check_protocol=0
      has_personal=0
      ;;
    *)
      disagree "unknown repository: $1 (want one of: content agents source)"
      return 1
      ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --message)
      message=${2:-}
      [[ -z $message ]] && { disagree "-m needs a message"; exit 2; }
      shift 2
      ;;
    --check) mode=check; shift ;;
    --dry-run) mode=dry; shift ;;
    --local) local_only=1; shift ;;
    --where) mode=where; shift ;;
    --state) mode=state; shift ;;
    --push-main)
      disagree "--push-main is gone: content is published, not pinned (see the header)"
      exit 2
      ;;
    -h | --help) usage; exit 0 ;;
    -*) disagree "unknown argument: $1 (try --help)"; exit 2 ;;
    *)
      if [[ -n $repo ]]; then
        disagree "one repository per call, not '$repo' and '$1'"
        exit 2
      fi
      repo=$1
      shift
      ;;
  esac
done

# `--local` narrows a check to what it can answer by itself; a publish always asks
# the remote, so the flag is refused everywhere else.
if [[ $local_only == 1 && $mode != check ]]; then
  disagree "--local belongs to --check: a publish asks the remote"
  exit 2
fi

[[ -n $repo ]] || { disagree "which repository: content, agents or source (try --help)"; exit 2; }
repo_config "$repo"

# The branch names are the server's, read from where they are defined rather than
# copied here: two names for one branch is how the pin drifted in the first place.
branch_of() { sed -n "s/^pub const $1: &str = \"\([^\"]*\)\";$/\1/p" "$source_of_truth" | head -1; }
official=$(branch_of OFFICIAL)
personal=$(branch_of PERSONAL)
[[ -n $official && -n $personal ]] || {
  disagree "cannot read OFFICIAL/PERSONAL from $source_of_truth"
  exit 2
}

personal_here=""
[[ $has_personal == 1 ]] && personal_here=$personal

if [[ $mode == where ]]; then
  printf 'path=%s\n' "$repo_path"
  printf 'official=%s\n' "$official"
  printf 'personal=%s\n' "$personal_here"
  printf 'remote=%s\n' "$remote"
  printf 'protocol=%s\n' "$check_protocol"
  printf 'publishes=%s\n' "$publishes"
  exit 0
fi

# The checkout itself: its git dir, asked for by path, and the refusal to answer for
# the repository above it when that path holds an entry git cannot read.
gitdir=""
worktree=""
open_repo() {
  local path=$1 entry
  [[ -d $path ]] || { disagree "no checkout at $path"; return 1; }
  worktree=$(cd "$path" && pwd -P)
  entry="$worktree/.git"
  if [[ -d $entry ]]; then
    gitdir=$entry
  elif [[ -f $entry ]]; then
    gitdir=$(sed -n 's/^gitdir: //p' "$entry" | tr -d '\r')
    case "$gitdir" in
      /* | [A-Za-z]:*) ;;
      *) gitdir="$worktree/$gitdir" ;;
    esac
  else
    disagree "$path holds no .git: not a checkout (for content: git submodule update --init ship)"
    return 1
  fi
  git --git-dir="$gitdir" --work-tree="$worktree" rev-parse --git-dir >/dev/null 2>&1 || {
    disagree "$entry does not hold a repository git can read (an interrupted clone leaves one)"
    return 1
  }
}
g() { git --git-dir="$gitdir" --work-tree="$worktree" "$@"; }

open_repo "$repo_path" || exit 1
head=$(g rev-parse --verify -q HEAD || true)
[[ -n $head ]] || { disagree "$repo has no commit yet"; exit 1; }
current=$(g symbolic-ref -q --short HEAD || true)
dirty=$(g status --porcelain)
origin_url=$(g config --get "remote.$remote.url" || true)

if [[ $mode == state ]]; then
  ahead=- behind=-
  if base=$(g rev-parse --verify -q "refs/remotes/$remote/$official"); then
    counts=$(g rev-list --left-right --count "$base...HEAD")
    behind=$(printf '%s\n' "$counts" | awk '{print $1}')
    ahead=$(printf '%s\n' "$counts" | awk '{print $2}')
  fi
  hooks=$(g config --get core.hooksPath || true)
  personal_ref=0
  if [[ -n $personal_here ]] && g rev-parse --verify -q "refs/heads/$personal_here" >/dev/null; then
    personal_ref=1
  fi
  printf 'path=%s\n' "$worktree"
  printf 'branch=%s\n' "${current:-detached}"
  printf 'head=%s\n' "$head"
  printf 'remote=%s\n' "$remote"
  printf 'official=%s\n' "$official"
  printf 'personal=%s\n' "$personal_here"
  printf 'personal_ref=%s\n' "$personal_ref"
  printf 'ahead=%s\n' "$ahead"
  printf 'behind=%s\n' "$behind"
  printf 'hooks=%s\n' "${hooks:-unset}"
  printf 'dirty=%s\n' "$(printf '%s' "$dirty" | grep -c '' || true)"
  printf 'conflicts=%s\n' "$(printf '%s\n' "$dirty" | grep -cE '^(DD|AU|UD|UA|DU|AA|UU)' || true)"
  printf 'origin=%s\n' "$origin_url"
  exit 0
fi

if [[ $publishes == 0 ]]; then
  disagree "$repo is not published by this script: its $official is this machine's line, and a checkpoint merges into it"
  disagree "what other machines read is published from: content, agents"
  exit 2
fi

# A field out of a `ship.toml`: one at the manifest (what the server reads), one in
# the content (what the content is).
field() { sed -n "s/^$2 = \"\{0,1\}\([^\"[:space:]]*\)\"\{0,1\}$/\1/p" "$1" 2>/dev/null | head -1; }

# The file a person just edited answers first: a check before the commit has to see
# what is about to be published, not what was published last.
local_protocol=$(field "$manifest" protocol)
content_protocol=""
if [[ $check_protocol == 1 ]]; then
  content_protocol=$(field "$repo_path/ship.toml" protocol)
  if [[ -z $content_protocol ]] && g cat-file -e "HEAD:ship.toml" 2>/dev/null; then
    content_protocol=$(g show HEAD:ship.toml | field /dev/stdin protocol || true)
  fi
fi

say "$repo HEAD  $head (${current:-detached})"
if [[ $check_protocol == 1 ]]; then
  say "protocol   content ${content_protocol:-<none>}, this build ${local_protocol:-<none>}"
fi
say "branches   official $official, personal ${personal_here:-<none>}, publishing ${current:-<detached>} -> ${official}"
[[ -n $dirty ]] && { say "uncommitted in $repo:"; printf '%s\n' "$dirty" | sed 's/^/      /'; }

# Whether it is published is a question for the remote, and a remote that does not
# answer leaves it open rather than answering no. See the exit codes.
# `--local` is the protocol question alone: no ls-remote, so a commit never
# waits on a network (scripts/hooks/pre-commit).
remote_sha=""
[[ $local_only == 1 ]] || remote_sha=$(g ls-remote "$remote" "refs/heads/$official" 2>/dev/null | cut -f1 || true)

verify() { # 0 agreed and published, 1 disagreement, 3 remote silent
  local drifted=0 published=1
  if [[ $check_protocol == 1 ]]; then
    if [[ -z $content_protocol ]]; then
      disagree "MISMATCH the content does not declare a protocol (ship/ship.toml: protocol = <n>)"
      drifted=1
    elif [[ $content_protocol != "$local_protocol" ]]; then
      disagree "MISMATCH the content speaks protocol $content_protocol; this build reads $local_protocol"
      disagree "         a server walks back to the newest commit it can read, or refuses to start"
      drifted=1
    fi
  fi
  if [[ $local_only == 1 ]]; then
    [[ $drifted == 0 ]] && say "in agreement"
    return $(( drifted != 0 ))
  fi
  # What would be published is this checkout's HEAD, rebased onto the official
  # branch: the official tip has to be an ancestor of it, and behind it.
  if [[ -z $remote_sha ]]; then
    published=0
  elif g merge-base --is-ancestor "$head" "$remote_sha" 2>/dev/null; then
    :
  elif g merge-base --is-ancestor "$remote_sha" "$head" 2>/dev/null; then
    disagree "NOT PUBLISHED $remote/$official is behind this checkout: run this without --check"
    drifted=1
  else
    disagree "MISMATCH $remote/$official and this checkout have diverged: pull or rebase first"
    drifted=1
  fi
  if [[ $drifted == 0 && $published == 1 ]]; then
    say "in agreement, and published"
    return 0
  fi
  if [[ $drifted == 0 ]]; then
    # Unreachable counts as "cannot tell" only when nothing else disagreed.
    disagree "UNVERIFIED $remote did not answer for $official: cannot tell whether this is published"
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
  if [[ -n $dirty && -z $message ]]; then
    say "(no -m: nothing would be committed)"
  elif [[ -n $dirty ]]; then
    say "would commit those changes on ${current:-$personal_here}, rebase onto $official, push to $remote/$official"
  else
    say "nothing to commit; the push would publish ${current:-$personal_here} on top of $official"
  fi
  exit 0
fi

# A submodule is cloned detached at whatever commit it was left on, so the personal
# branch may not be checked out here yet.
if [[ -z $current ]]; then
  say "$repo is detached; working on a new $personal_here"
  g checkout -B "$personal_here" >/dev/null
elif [[ $current != "$personal_here" && $current != "$official" ]]; then
  disagree "$repo is on '$current': publish from '$official' (a commit that belongs to everyone),"
  disagree "or from '$personal_here' (this machine's own, rebased onto '$official' first); switch first"
  exit 1
fi

if [[ -n $dirty ]]; then
  [[ -n $message ]] || { disagree "-m is required when $repo has changes; do not publish a message you did not write"; exit 2; }
  g add -A
  # Staged is not the same question as changed: `git status` reports a line-ending
  # difference that normalisation then absorbs, and a commit of nothing fails.
  if g diff --cached --quiet; then
    say "nothing to commit in $repo (the index matches HEAD)"
  else
    g commit -q -m "$message"
    say "committed $(g rev-parse --short HEAD): $(printf '%s' "$message" | head -c 60)"
  fi
elif [[ -z $message ]]; then
  say "nothing to commit in $repo"
fi

# The rebase that comes before the push: publishing a personal commit means putting
# it on top of what official is now, never a merge nobody can read later.
if [[ -n $remote_sha ]]; then
  # `ship/.git` may be a file (a submodule), which is why the git dir was resolved.
  if ! fetched=$(g fetch "$remote" "+refs/heads/$official:refs/remotes/$remote/$official" 2>&1); then
    printf '%s\n' "$fetched" | sed 's/^/    /' >&2
    disagree "$remote did not answer; nothing was pushed"
    exit 1
  fi
  if ! g rebase -q "refs/remotes/$remote/$official"; then
    g rebase --abort >/dev/null 2>&1 || true
    disagree "${current:-$personal_here} conflicts with $remote/$official; the rebase was undone, nothing was pushed"
    disagree "resolve it by hand, then run this again"
    exit 1
  fi
  say "rebased $(g symbolic-ref -q --short HEAD || printf '%s' "$personal_here") onto $remote/$official ($(g rev-parse --short "refs/remotes/$remote/$official"))"
fi

if ! pushed=$(g push "$remote" "HEAD:refs/heads/$official" 2>&1); then
  printf '%s\n' "$pushed" | sed 's/^/    /' >&2
  disagree "the push failed; nothing was published"
  exit 1
fi
say "pushed $official to $remote ($(printf '%s' "$pushed" | tail -1 | tr -s ' '))"
# The local official branch follows what was just published, so the next check and
# the next fetch agree with the remote.
g fetch -q "$remote" "+refs/heads/$official:refs/heads/$official" 2>/dev/null || true

# Asked again, of the remote: the answer that matters is the one after the push.
head=$(g rev-parse HEAD)
remote_sha=$(g ls-remote "$remote" "refs/heads/$official" 2>/dev/null | cut -f1 || true)
code=0
verify || code=$?
# A server installs the content at its start, so a running machine sees this after
# `evolve`. The build is redone only when this build's own protocol changed.
[[ $repo == content ]] && say "reminder: a running server installs the content at its next start (evolve), not now"
exit "$code"
