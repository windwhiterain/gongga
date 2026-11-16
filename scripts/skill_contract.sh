#!/usr/bin/env bash
# Hold every skill's prose to the contract that makes it portable.
#
#   scripts/skill_contract.sh            # the skills this repository owns
#   scripts/skill_contract.sh path ...   # the skills named, e.g. from a hook
#
# A skill's body is read by whatever harness installed it, so a line that runs its own
# script has to be spelled in the one way every harness resolves: relative to the
# directory the `SKILL.md` was read from. A path under this harness's home
# (`$GONGGA_HOME/skills/...`), under the community clone (`gongga-agents/skills/...`), or
# under `~/.agents/skills/<name>/` works here and nowhere else, and it breaks here too when
# the home moves.
#
# The one machine-read field this harness adds lives where the specification allows an
# extension, `metadata.entry`, and it names a file inside that skill's directory.
#
# Exit: 0 every skill holds, 1 at least one does not.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root" || exit 1

if [[ $# -gt 0 ]]; then
  files=("$@")
else
  # Both layers this repository owns: the pinned content and this checkout's own. A shell
  # glob, not `find`: on a machine whose `PATH` starts with Windows' own tools, `find` is
  # `C:\Windows\system32\find.exe`, which refuses these arguments -- and with its stderr
  # discarded that read as "no skills, nothing to check, PASS".
  files=()
  shopt -s nullglob
  for file in ship/skills/*/SKILL.md .agents/skills/*/SKILL.md; do files+=("$file"); done
  shopt -u nullglob
fi

failed=0
checks=0
for file in "${files[@]}"; do
  [[ -f "$file" ]] || continue
  checks=$((checks + 1))
  directory=$(dirname "$file")

  while IFS= read -r hit; do
    line=${hit%%:*}
    echo "FAIL $file:$line names an installed path; spell it from this skill's directory:" >&2
    echo "     ${hit#*:}" >&2
    failed=1
  # Every spelling of "where this copy happens to be": this harness's home, the community
  # clone, and the machine's own -- the last one needs two segments, because a body may
  # *describe* a bucket (`~/.agents/skills/<name>/`) but must not *invoke* by that path.
  done < <(grep -nE '\$\{?GONGGA_HOME\}?/skills/|gongga-agents/skills/|~/?\.agents/skills/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+|\$HOME[\\/]\.agents[\\/]skills[\\/][A-Za-z0-9._-]+' "$file" || true)

  # `metadata:` then an indented `entry:`: the same two lines the server reads.
  entry=$(awk '
    /^---[[:space:]]*$/ { fence++; next }
    fence != 1 { next }
    /^[^[:space:]]/ { section = $0; sub(/:.*/, "", section); next }
    section == "metadata" && /^[[:space:]]+entry:/ {
      sub(/^[[:space:]]+entry:[[:space:]]*/, ""); gsub(/^"|"$/, ""); print; exit
    }
  ' "$file")
  [[ -z "$entry" ]] && continue
  case "$entry" in
  /* | *..* | ?:*)
    echo "FAIL $file declares entry: $entry, which is not a path inside the skill" >&2
    failed=1
    continue
    ;;
  esac
  if [[ ! -f "$directory/$entry" ]]; then
    echo "FAIL $file declares entry: $entry, and $directory has no such file" >&2
    failed=1
  fi
done

if [[ $failed != 0 ]]; then
  echo "FAIL $checks skill(s) checked" >&2
  exit 1
fi
# A gate that checked nothing is a gate that did not run: say so rather than pass.
if [[ $checks == 0 ]]; then
  echo "FAIL 0 skill(s): no SKILL.md under ship/skills or .agents/skills -- this check read nothing" >&2
  exit 1
fi
echo "PASS $checks skill(s): no installed path, and every entry names a file"
