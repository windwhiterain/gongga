#!/usr/bin/env bash
# Publish the shipped content and record the commit it now pins, in one step.
#
#   scripts/ship.sh -m "heir: name the mentor in the guide"        # commit, push, pin
#   scripts/ship.sh --check                                         # does anything disagree?
#   scripts/ship.sh --dry-run -m "..."                              # what would happen
#   scripts/ship.sh -m "..." --push-main                            # and commit the pin here
#
# Why a script and not a habit: `crates/server/ship.toml` is what a server installs
# and `ship/` is what a person edits. Two names for one commit drift the moment
# somebody forgets the second half, and the failure is silent -- a home installs the
# old prose and the log line says everything is fine.
#
# So this publishes, then insists that three things agree: the submodule's HEAD, the
# manifest, and what the remote answers for refs/heads/main. The last one is the
# point of a pin: a fresh machine has to be able to fetch that commit, so a sha that
# only exists in this working tree is not a release.
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

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --message)
      message=${2:-}
      [[ -z "$message" ]] && { echo "-m needs a message" >&2; exit 2; }
      shift 2
      ;;
    --check)   mode=check; shift ;;
    --dry-run) mode=dry; shift ;;
    --push-main) push_main=1; shift ;;
    -h | --help) sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
  esac
done

[[ -d "$ship/.git" || -f "$ship/.git" ]] || {
  echo "no ship/ here: git submodule update --init" >&2
  exit 1
}

# What the remote thinks main is, if it answers at all.
remote_sha=$(git -C "$ship" ls-remote "$remote" "refs/heads/$branch" 2>/dev/null | cut -f1 || true)
local_sha=$(git -C "$ship" rev-parse HEAD)
dirty=$(git -C "$ship" status --porcelain)

say() { printf '  %s\n' "$*"; }

report() {
  say "ship HEAD   $local_sha"
  say "remote $branch  ${remote_sha:-<nothing advertised>}"
  say "manifest    $(sed -n 's/^commit = "\([0-9a-f]*\)"/\1/p' "$manifest")"
  [[ -n "$dirty" ]] && { say "uncommitted in ship/:"; printf '%s\n' "$dirty" | sed 's/^/      /'; }
  say "this repo   $(git -C "$root" status --porcelain -- ship crates/server/ship.toml | head -3 | tr '\n' ' ')"
}

pinned=$(sed -n 's/^commit = "\([0-9a-f]*\)"/\1/p' "$manifest")
agree=1
[[ "$local_sha" == "$pinned" ]] || { echo "  MISMATCH ship HEAD is not what ship.toml pins" >&2; agree=0; }
if [[ -n "$remote_sha" ]]; then
  if [[ "$remote_sha" != "$local_sha" ]]; then
    if git -C "$ship" merge-base --is-ancestor "$remote_sha" "$local_sha" 2>/dev/null; then
      echo "  NOT PUBLISHED ship HEAD is ahead of $remote/$branch: run this without --check" >&2
    else
      echo "  MISMATCH $remote/$branch has commits ship/ does not -- diverged, do not publish over it" >&2
    fi
    agree=0
  fi
else
  echo "  MISMATCH $remote advertises no $branch: the pin is not fetchable" >&2; agree=0
fi

if [[ $mode == check ]]; then
  report
  if [[ $agree == 1 ]]; then
    echo "  in agreement"
    exit 0
  fi
  exit 1
fi

if [[ $mode == dry ]]; then
  report
  if [[ -z "$message" ]]; then echo "  (no -m: nothing would be committed)"; fi
  if [[ -n "$dirty" && -n "$message" ]]; then
    echo "  would commit those changes, push $branch to $remote, write the sha into ship.toml"
  elif [[ -z "$dirty" ]]; then
    echo "  nothing to commit in ship/; the pin would only be rewritten if it disagrees"
  fi
  exit 0
fi

# A submodule is cloned detached at the pin, so "the branch" may not exist yet.
if ! git -C "$ship" symbolic-ref -q "HEAD"; then
  echo "  ship/ is detached at the pin; working on a new $branch" >&2
  git -C "$ship" checkout -B "$branch" >/dev/null
  local_sha=$(git -C "$ship" rev-parse HEAD)
fi
current=$(git -C "$ship" symbolic-ref --short HEAD)
if [[ "$current" != "$branch" ]]; then
  echo "  ship/ is on '$current', not '$branch'; switch first -- this script does not guess which branch is the release" >&2
  exit 1
fi

# Never push over something nobody pulled: a rejected push mid-script leaves the
# commit made and the manifest unwritten, which is exactly the drift this is for.
if [[ -n "$remote_sha" && "$remote_sha" != "$local_sha" ]] &&
   ! git -C "$ship" merge-base --is-ancestor "$remote_sha" "$local_sha" 2>/dev/null; then
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
  pushed=$(git -C "$ship" push "$remote" "HEAD:refs/heads/$branch" 2>&1) || {
    printf '%s
' "$pushed" | sed 's/^/    /' >&2
    echo "  the push failed, so ship.toml still names the last published commit" >&2
    exit 1
  }
  echo "  pushed $branch to $remote ($(printf '%s' "$pushed" | tail -1 | tr -s ' '))"
fi

# Now the manifest, and only the manifest's value: the file's comments and the url
# are not this script's business.
sed -i "s|^commit = \"[0-9a-f]*\"|commit = \"$local_sha\"|" "$manifest"
[[ $(sed -n "s/^commit = \"$local_sha\"/ok/p" "$manifest" | grep -c ok) == 1 ]] || {
  echo "  ship.toml does not read back as pinning $local_sha" >&2
  exit 1
}
git -C "$root" add -- ship crates/server/ship.toml
echo "  pinned $local_sha (ship.toml + the submodule commit are staged here)"

if [[ ${push_main:-0} == 1 ]]; then
  if git -C "$root" diff --cached --quiet -- ship crates/server/ship.toml; then
    echo "  nothing staged in this repo; no commit made"
  else
    git -C "$root" commit -q -m "ship: pin ${local_sha:0:10}"
    git -C "$root" push
    echo "  committed and pushed this repo's pin"
  fi
else
  echo "  not pushed here: the pin is staged; commit it with the work that needed it, or rerun with --push-main"
fi

# The pin only reaches a running server through a rebuild: it is compiled in.
echo "  reminder: ship.toml is include_str! -- build -p gongga-server (and evolve, to run on it)"
