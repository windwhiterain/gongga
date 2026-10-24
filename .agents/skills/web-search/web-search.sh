#!/usr/bin/env bash
# Live web search and page fetch through DeepSeek's own tools API.
#
#   web-search.sh search "query" [count]     # ranked sources
#   web-search.sh fetch <url>                # one page as markdown
#   web-search.sh search "query" --content   # sources plus page bodies
#
# Costs nothing but an HTTP call: this is a REST endpoint, not a model turn.
# Needs no model that supports server-side search, so it works on any session.
set -uo pipefail

ROSTER="${DEEPSEEK_ROSTER:-$HOME/.agents/llm.toml}"
BASE="${DEEPSEEK_TOOLS_URL:-https://api.deepseek.com/v1/tools}"

die() { printf '%s\n' "$*" >&2; exit 1; }

# The key: the environment wins, then the first literal key in the roster. The
# roster is the machine's own record, so a missing key there is worth naming.
api_key() {
  if [ -n "${DEEPSEEK_API_KEY:-}" ]; then printf '%s' "$DEEPSEEK_API_KEY"; return 0; fi
  [ -r "$ROSTER" ] || { die "no DEEPSEEK_API_KEY and no roster at $ROSTER"; return 1; }
  local k
  k=$(sed -n 's/^[[:space:]]*key[[:space:]]*=[[:space:]]*"\(sk-[^"]*\)".*/\1/p' "$ROSTER" | head -n1)
  [ -n "$k" ] || { die "no DEEPSEEK_API_KEY and no key = \"sk-...\" in $ROSTER"; return 1; }
  printf '%s' "$k"
}

# Schannel's revocation check hangs behind some corporate proxies, so Windows curl
# gets --ssl-no-revoke. Every other curl rejects that flag outright ("option not
# recognised"), so it is probed for rather than assumed.
CURL_OPTS=(-sS -m 120)
if curl --help all 2>/dev/null | grep -q -- '--ssl-no-revoke'; then
  CURL_OPTS+=(--ssl-no-revoke)
fi

# One JSON body in, one JSON body out; HTTP status on stderr when it is not 200.
post() {
  local path="$1" body="$2" key
  key=$(api_key) || return 1
  local out status
  out=$(curl "${CURL_OPTS[@]}" -w '\n%{http_code}' "$BASE/$path" \
    -H "Authorization: Bearer $key" -H 'content-type: application/json' \
    -d "$body" 2>&1) || die "request to $BASE/$path failed: $out"
  status=${out##*$'\n'}
  out=${out%$'\n'*}
  if [ "$status" != "200" ]; then
    printf 'HTTP %s from %s/%s\n%s\n' "$status" "$BASE" "$path" "$out" >&2
    return 1
  fi
  printf '%s' "$out"
}

render() { python -c "$1" ; }

cmd_search() {
  local query="${1-}"
  # Not ${1:?usage...}: that prints bash's own "line N:" prefix, which reads like an
  # internal failure rather than a usage mistake.
  if [ -z "$query" ]; then
    die 'search needs a query: web-search.sh search "query" [count] [--content]'
  fi
  shift
  local count=6 content=false
  for arg in "$@"; do
    case "$arg" in
      --content) content=true ;;
      ''|*[!0-9]*) die "count must be a number, got: $arg" ;;
      *) count="$arg" ;;
    esac
  done
  local body
  body=$(python -c '
import json,sys
print(json.dumps({"query": sys.argv[1], "count": int(sys.argv[2]), "need_content": sys.argv[3]=="true"}))
' "$query" "$count" "$content")
  local json
  json=$(post search "$body") || exit 1
  printf '%s' "$json" | render '
import json,sys,textwrap
raw = sys.stdin.read()
try:
    d = json.loads(raw)
except ValueError:
    sys.exit("the endpoint did not return JSON:\n" + raw[:800])
hits = d.get("results") or []
if not hits:
    sys.exit("no results")
for i, r in enumerate(hits, 1):
    title = (r.get("title") or "").strip() or "(untitled)"
    url = (r.get("url") or "").strip()
    site = (r.get("site_name") or "").strip()
    date = (r.get("date") or "").strip()
    print(f"{i}. {title}")
    print(f"   {url}")
    meta = " | ".join(x for x in (site, date) if x)
    if meta:
        print(f"   {meta}")
    text = (r.get("summary") or r.get("snippet") or "").strip()
    if text:
        for line in textwrap.wrap(text, 110)[:6]:
            print(f"   {line}")
    body = (r.get("content") or "").strip()
    if body:
        print("   --- content ---")
        for line in body.splitlines()[:40]:
            print(f"   {line}")
    print()
'
}

cmd_fetch() {
  local url="${1:?usage: web-search.sh fetch <url>}"
  local body
  body=$(python -c 'import json,sys; print(json.dumps({"url": sys.argv[1]}))' "$url")
  local json
  json=$(post fetch "$body") || exit 1
  printf '%s' "$json" | render '
import json,sys
raw = sys.stdin.read()
try:
    d = json.loads(raw)
except ValueError:
    sys.exit("the endpoint did not return JSON:\n" + raw[:800])
title = (d.get("title") or "").strip()
text = d.get("content") or ""
if title:
    print(title)
    print()
if not text.strip():
    sys.exit("the endpoint returned no content for this URL")
print(text)
'
}

case "${1:-}" in
  search) shift; cmd_search "$@" ;;
  fetch)  shift; cmd_fetch "$@" ;;
  *) die "usage: web-search.sh search \"query\" [count] [--content] | web-search.sh fetch <url>" ;;
esac
