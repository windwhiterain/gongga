#!/usr/bin/env bash
# `run_skill` on real processes: a package's call arrives as one JSON line on stdin, a
# directory that is not a package is refused with the sentence that says what to make,
# and the package's own `import gongga` resolves against this home's `lib/`.
#
# One line of verdict at the end; `-v` names the server log.
set -uo pipefail

# A path this machine opens: bash hands out `/c/...`, and the server reads `C:/...`.
native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
compact() { python -c 'import sys; print(sys.argv[1].replace(chr(92), "").replace("/", "").lower())' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${SKILL_CALL_SELFTEST_BIN:-$root/target/debug}"
work="$root/.dev/selftest"
home="$work/skill-call-home"
checkout="$work/skill-call-checkout"
log="$work/skill-call.log"
verbose=${1:-}
started=$(date +%s)

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build" >&2; exit 1; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
# The pid the server wrote for THIS home, never `$!`: bash's is an MSYS number.
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
dial_ready() {
  python -c 'import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 1)' "$1"
}
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }
# Every process serving THIS home, whatever a previous run remembers: the home has
# several spellings, and the match is the home's own path, never the image name, which
# every other server on this box shares.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

cleanup() {
  local pid; pid=$(child_pid)
  alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  sleep 1
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
  rm -rf "$checkout"
}
trap cleanup EXIT

left=$(serving_home)
if [[ -n "${left// /}" ]]; then
  echo "a previous run left $(echo $left) serving $home; stopping it" >&2
  for pid in $left; do taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 2
fi
rm -rf "$home" "$checkout"; mkdir -p "$home"
: > "$log"

# The shapes, in the project bucket of a checkout of this run's own: a package, a body, and
# a directory that still declares an entry -- which this build reads nowhere, so it is
# refused like the body it also is.
mkdir -p "$checkout/.agents/skills/pkg" "$checkout/.agents/skills/body" "$checkout/.agents/skills/entryonly"
cat > "$checkout/.agents/skills/pkg/SKILL.md" <<'MD'
---
name: pkg
description: a package skill, which the server runs
---
# pkg

`probe(text)` answers with the text it was given and the `gongga` it imported.
MD
cat > "$checkout/.agents/skills/pkg/__init__.py" <<'PY'
def probe(text=""):
    import gongga

    return f"pkg:{text}|{gongga.__file__}"
PY
cat > "$checkout/.agents/skills/body/SKILL.md" <<'MD'
---
name: body
description: prose only, which the server refuses
---
# body

Nothing here runs.
MD
cat > "$checkout/.agents/skills/entryonly/SKILL.md" <<'MD'
---
name: entryonly
description: a declared entry, which the server refuses
metadata:
  entry: run.sh
---
# entryonly

The file beside this one is what the declaration used to name.
MD
printf '#!/usr/bin/env bash\necho entry-ran\n' > "$checkout/.agents/skills/entryonly/run.sh"

# The content lands in this home's `lib/` at start: the package contract and the client
# a skill imports. Cloned rather than copied from a live home, because the package
# scenario's answer is this checkout's `python/` tree and not a neighbour's.
if ! git clone -q https://github.com/windwhiterain/gongga-ship.git "$home/ship" >>"$log" 2>&1; then
  echo "could not clone the content repository into $home/ship" >&2; exit 1
fi

# One model, for a session to name: no request is made here, so the endpoint is a port
# nothing listens on.
dead_port=$(free_port)
cat > "$work/skill-call-roster.toml" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "FAKE_API_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$dead_port/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Fake Flash"
  tier = "flash"
  context = 100000
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$work/skill-call-roster.toml"
export FAKE_API_KEY=sk-fake

port=$(free_port)
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  "$bin/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; sed -n '1,20p' "$log" >&2; exit 1; }
say "server log: $log"
cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")

[[ -f "$home/ship/python/gongga_skill.py" && -f "$home/ship/python/gongga.py" ]]
ok "the content installs the package contract and the client into lib/" $?

created=$("${cli[@]}" '{"name":"set_session","args":{"id":"reader","model":"fake/flash-fake","cwd":"'"$(native "$checkout")"'"}}' 2>&1)
[[ "$created" == *'"ok":true'* ]]
ok "a session stands in the checkout that holds the three skills" $?

# (a) the package runs, and what it answers is what it printed
out=$("${cli[@]}" '{"name":"run_skill","args":{"id":"pkg","session":"reader","cmd":{"name":"probe","args":{"text":"hello"}}}}' 2>&1)
say "$out"
[[ "$out" == *"pkg:hello|"* && "$out" == *"[exit status 0]"* ]]
ok "a package's function is called with keyword arguments and its output returned" $?

# (d) the package's own `import gongga` is this home's client, not a launcher's. The
# answer travels as a JSON line, so a separator is an escape there and a slash in the
# path: the comparison drops separators rather than normalising them.
flat=$(compact "$out")
want=$(compact "$(native "$home/ship/python/gongga.py")")
imported=1
[[ "$flat" == *"$want"* ]] && imported=0
[[ $imported == 0 ]] || say "want=$want got=${flat:0:400}"
ok "the package imports gongga from this home's lib/" $imported

# (b) a body is refused: one sentence naming the package to make, and the SKILL.md it read
out=$("${cli[@]}" '{"name":"run_skill","args":{"id":"body","session":"reader","cmd":{"name":"anything","args":{}}}}' 2>&1)
say "$out"
[[ "$out" == *"only as a Python call"* && "$out" == *"__init__.py"* && "$out" == *"SKILL.md"* && "$out" != *"pkg:"* ]]
ok "a body is refused, naming the package to make and the SKILL.md it read" $?

# (c) a declared entry is refused the same way: no code of this build reads the declaration,
# so the file it names is not run either.
out=$("${cli[@]}" '{"name":"run_skill","args":{"id":"entryonly","session":"reader","cmd":{"name":"anything","args":{}}}}' 2>&1)
say "$out"
[[ "$out" == *"only as a Python call"* && "$out" == *"__init__.py"* && "$out" == *"SPEC"* && "$out" != *"entry-ran"* ]]
ok "a declared entry is refused, naming the package that would run instead" $?

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
elapsed=$(( $(date +%s) - started ))
if [[ $fails == 0 ]]; then
  printf 'PASS · skill-call · %ss · %s\n' "$elapsed" "$sha"
else
  printf 'FAIL · skill-call · %d scenario(s) · %ss · %s\n' "$fails" "$elapsed" "$sha"; exit 1
fi
