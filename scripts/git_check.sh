#!/usr/bin/env bash
# Read the three checkouts this repository's git flow governs, and say whether each
# stands in a shape that flow allows.
#
#   scripts/git_check.sh     # 0 every checkout is allowed, 1 at least one is not
#
# Read-only: nothing is fetched, nothing is written, and no remote is asked. Every
# git call names its git dir and work tree -- through `publish.sh <repo> --state`,
# which is the one place that resolves them -- because `git -C` answers for the
# repository *above* a checkout whose `.git` git cannot read. See AGENTS.md, "主仓上游".
#
# One line per checkout, then one verdict:
#
#   <repo> branch=<b> vs <remote>/<official> +<ahead>/-<behind> hooks=<h> dirty=<n> <ok|why>
#
# The shapes, and why each repository's are what they are:
#
#   source   `main`, or a named branch `<prefix>/<name>`; `scripts/hooks` installed.
#            `main` is this machine's own line: ahead of origin/main is expected, and
#            it is never published.
#   content  `main` (a commit that belongs to everyone) or `local` (this machine's,
#            rebased onto it). A checkout with no `local` yet, nothing uncommitted and
#            no branch under it is `pristine` -- what `git submodule update --init`
#            leaves -- and holds nothing to publish.
#   agents   the same two branches, for the same reason.
#
# What fails: a branch the flow does not allow, a conflict left in the tree, hooks not
# installed where the flow needs them, and an upstream that names another of the three
# repositories -- the shape of the mistake that pointed the source repository's origin
# at the content's.
#
# This is not `publish.sh --check`. That one answers "is what I would publish
# published", which is false for any work in progress; this asks whether publishing is
# the next step: the branch may publish, the tree is not half-merged, and each
# checkout is wired to its own upstream.
set -uo pipefail

# `git status` refreshes the index unless told not to: a check that rewrote the index
# would not be read-only.
export GIT_OPTIONAL_LOCKS=0

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
publish="$root/scripts/publish.sh"
repos="source content agents"
fails=0

field_of() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

# The upstream names each checkout must not point at. The source repository's origin
# is gongga.git and the ship URL belongs to the content alone; a checkout that names
# another one of the three publishes someone else's content from here.
forbidden_of() {
  case "$1" in
    source) printf 'gongga-ship gongga-agents\n' ;;
    content) printf 'gongga-agents /gongga.git\n' ;;
    agents) printf 'gongga-ship /gongga.git\n' ;;
  esac
}

for repo in $repos; do
  where=$(bash "$publish" "$repo" --where 2>/dev/null)
  if [[ -z $where ]]; then
    printf '%-8s unknown repository\n' "$repo"
    fails=$((fails + 1))
    continue
  fi
  if ! state=$(bash "$publish" "$repo" --state 2>&1); then
    printf '%-8s not readable: %s\n' "$repo" "$(printf '%s' "$state" | tr '\n' ' ' | tr -s ' ' | sed 's/^ //')"
    fails=$((fails + 1))
    continue
  fi

  official=$(field_of "$where" official)
  personal=$(field_of "$where" personal)
  remote=$(field_of "$where" remote)
  path=$(field_of "$state" path)
  branch=$(field_of "$state" branch)
  ahead=$(field_of "$state" ahead)
  behind=$(field_of "$state" behind)
  hooks=$(field_of "$state" hooks)
  dirty=$(field_of "$state" dirty)
  conflicts=$(field_of "$state" conflicts)
  personal_ref=$(field_of "$state" personal_ref)
  origin=$(field_of "$state" origin)

  why=""
  verdict=ok
  if [[ $branch == detached ]]; then
    if [[ $dirty != 0 || $conflicts != 0 || $personal_ref != 0 ]]; then
      why="detached, and it holds work no branch can publish"
    else
      verdict="ok (pristine)"
    fi
  elif [[ $repo == source ]]; then
    case "$branch" in
      "$official" | */*) ;;
      *) why="'$branch' is neither $official nor a named branch" ;;
    esac
  elif [[ $branch != "$official" && $branch != "$personal" ]]; then
    why="'$branch' is neither $official nor $personal"
  fi

  if [[ -z $why && $conflicts != 0 ]]; then
    why="$conflicts conflicted path(s) left in the tree"
  fi
  if [[ -z $why && $repo == source && $hooks != scripts/hooks ]]; then
    why="core.hooksPath is $hooks, want scripts/hooks"
  fi
  if [[ -z $why ]]; then
    for other in $(forbidden_of "$repo"); do
      case "$origin" in
        *"$other"*)
          why="origin is $origin: that is another checkout's upstream"
          break
          ;;
      esac
    done
  fi

  if [[ -n $why ]]; then
    fails=$((fails + 1))
    printf '%-8s branch=%-20s vs %s/%s +%s/-%s  hooks=%s  dirty=%s  %s\n' \
      "$repo" "$branch" "$remote" "$official" "$ahead" "$behind" "$hooks" "$dirty" "$why"
    printf '%-8s at %s\n' "" "$path"
  else
    printf '%-8s branch=%-20s vs %s/%s +%s/-%s  hooks=%s  dirty=%s  %s\n' \
      "$repo" "$branch" "$remote" "$official" "$ahead" "$behind" "$hooks" "$dirty" "$verdict"
  fi
done

if [[ $fails == 0 ]]; then
  printf 'PASS\n'
  exit 0
fi
printf 'FAIL %s\n' "$fails"
exit 1
