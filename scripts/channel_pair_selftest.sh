#!/usr/bin/env bash
# One relay, one server, and the protocol between them: a channel start that meets a
# relay of another protocol has to end in a sentence, not in a deadline.
#
# Three scenarios on one home, because they are one story:
#
#   1. the copy this server installed holds a channel;
#   2. a copy that has gone bad is replaced from beside the server, and the channel
#      starts on the second attempt -- the install SPEC asks for, done at the moment
#      the pair is found wanting;
#   3. a copy that cannot be replaced at all fails the start at once, with the relay's
#      own words, instead of `START_DEADLINE` later with none.
#
# One line of verdict at the end; `-v` keeps the scratch.
set -uo pipefail

native() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="${GONGGA_BIN:-$root/target/debug}"
tag="pair-$$"
work="$root/.dev/selftest/$tag"
home="$work/home"
log="$work/server.log"
verbose=${1:-}
began=$(date +%s)

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-relay" >&2; exit 1; }
server_bin="$bin/gongga-server"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"

free_port() {
  python -c 'import socket
s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}
wait_listening() {
  local seen
  seen=$(grep -c "listening (UDP) local=127.0.0.1:$1" "$log" 2>/dev/null)
  seen=${seen:-0}
  for _ in $(seq 1 300); do
    [[ "$(grep -c "listening (UDP) local=127.0.0.1:$1" "$log" 2>/dev/null)" -gt "$seen" ]] && return 0
    sleep 0.2
  done
  return 1
}
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }

server_pid=""
cleanup() {
  for pid in "$server_pid" "$(child_pid)"; do
    alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  done
  sleep 1
  [[ "$verbose" == "-v" ]] && return 0
  rm -rf "$home" 2>/dev/null
  rm -rf "$work" 2>/dev/null
}
trap cleanup EXIT

rm -rf "$home"; mkdir -p "$work" "$home"
# This checkout's own content: a home with none reaches for the network.
git clone --quiet "$root/ship" "$home/ship" 2>/dev/null || cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"
export GONGGA_HOME="$home"

# A server, started the way the scenario needs it: `from` is the program the shell
# starts, and it decides where `refresh_relay` looks for the copy it would install.
up() {
  port=$(free_port)
  export GONGGA_HOME="$home"
  rm -f "$home/server.lock"
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
      -u GONGGA_API_KEY_ENV -u GONGGA_SESSION -u GONGGA_RELAY_BIN \
      "$1" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
  for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
  server_pid=$(child_pid)
  wait_listening "$port" || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
  cli=("$home/bin/gongga-cli" --home "$home" --server "127.0.0.1:$port")
  "${cli[@]}" '{"name":"set_session","args":{"id":"pair","cwd":"'"$(native "$root")"'"}}' >/dev/null 2>&1
}

down() {
  local pid
  pid=$(child_pid)
  [[ -n "$pid" ]] && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  for _ in $(seq 1 100); do alive "$pid" || break; sleep 0.2; done
  sleep 0.5
}

# One channel start: what the command layer answered (`CH_ANSWER`), how long it took
# (`CH_MS`), and the status it exited with (`CH_CODE`). A refusal is silent on stdout --
# the command layer prints events only -- so the status is half of every answer here.
start_channel() {
  local began_ms ended_ms
  began_ms=$(date +%s%3N)
  CH_ANSWER=$("${cli[@]}" '{"name":"new_channel","args":{"id":"'"$1"'","name":"bash","session":"pair"}}' 2>&1)
  CH_CODE=$?
  ended_ms=$(date +%s%3N)
  CH_MS=$((ended_ms - began_ms))
}

# How many channel directories there are: a start that fails leaves neither a row nor a
# directory, so this counts both kinds of litter at once.
channels() { ls -d "$home"/channels/ch-* 2>/dev/null | wc -l | tr -d ' '; }

relay="$bin/gongga-relay.exe"; [[ -x "$relay" ]] || relay="$bin/gongga-relay"
protocol=$(sed -n 's/^pub const PROTOCOL: u32 = \([0-9][0-9]*\);/\1/p' "$root/crates/relay/src/relay.rs")
[[ -n "$protocol" ]] || { echo "no PROTOCOL in crates/relay/src/relay.rs" >&2; exit 1; }

# 0. The rule at the program itself, with no server in the way: a relay is held to the
#    protocol, never to the commit. `deadbeef` is a build no one has; it must not matter.
probe() {
  local dir="$work/probe-$1"
  rm -rf "$dir"; mkdir -p "$dir"
  printf '{"program":"bash","args":["-lc","echo pair-ok"],"cwd":"%s","env":[]}' "$(native "$root")" > "$dir/cmd"
  : > "$dir/in"
  shift
  "$relay" --relay "$dir" "$@" >"$dir/words" 2>&1
}
probe same-build --protocol "$protocol" --build deadbeef; code=$?
ok "another build of the same protocol is a pair" "$([[ $code == 0 ]] && echo 0 || echo 1)"
probe silent --build deadbeef; code=$?
ok "a server that names no protocol is accepted, the way an older one asks" \
  "$([[ $code == 0 ]] && echo 0 || echo 1)"
probe other-protocol --protocol "$((protocol + 1))"; code=$?
ok "another protocol is refused" "$([[ $code == 3 ]] && echo 0 || echo 1)"
ok "and the refusal names both protocols" \
  "$(grep -q "protocol $protocol" "$work/probe-other-protocol/words" \
     && grep -q "speaks $((protocol + 1))" "$work/probe-other-protocol/words" && echo 0 || echo 1)"

up "$server_bin"

# 1. The copy in `bin` goes bad before anything asks it to hold a channel -- what a relay
#    of another protocol looks like to a server that finds it there. The start has to
#    find the copy beside the server, install it, and try again.
#    `rm` first: an install is a hard link, and truncating it in place would take the
#    build tree's own copy with it -- which is a fact about links, not about pairs.
rm -f "$home/bin/gongga-relay.exe"
printf 'not a program\n' > "$home/bin/gongga-relay.exe"
start_channel one
say "repaired: $(grep -v '^{"t":"event"' <<<"$CH_ANSWER" | head -2)"
ok "a copy that cannot work is replaced, and the channel still starts" \
  "$(grep -q 'started: bash' <<<"$CH_ANSWER" && echo 0 || echo 1)"
ok "what the home's bin holds is a program again" \
  "$("$home/bin/gongga-relay.exe" --version >/dev/null 2>&1 && echo 0 || echo 1)"

# 2. The pair as it now stands: another channel starts on the first attempt.
start_channel two
say "healthy: $(grep -v '^{"t":"event"' <<<"$CH_ANSWER" | head -2)"
ok "the installed pair holds a channel" "$(grep -q 'started: bash' <<<"$CH_ANSWER" && echo 0 || echo 1)"

# 3. Now nothing is beside the server but a copy in `bin` that is not a program: there is
#    nothing to install, so the start must say so at once rather than sit out
#    `START_DEADLINE` and leave a directory that never answers.
#    A home of its own, because a relay of the run above still holds `bin`'s copy on
#    Windows -- a pinned copy is the other half of this story, and it is not this one.
down
home="$work/home2"
mkdir -p "$home/bin"
cp "$server_bin" "$home/bin/"
printf 'not a program\n' > "$home/bin/gongga-relay.exe"
cp -r "$work/home/ship" "$home/ship" 2>/dev/null
[[ "$(stat -c %s "$home/bin/gongga-relay.exe")" == 14 ]] || {
  echo "the copy in $home/bin could not be written as a non-program" >&2; exit 1; }
up "$home/bin/gongga-server.exe"
before=$(channels)
start_channel three
say "unfixable: $(grep -v '^{"t":"event"' <<<"$CH_ANSWER" | head -3) (${CH_MS}ms, exit ${CH_CODE})"
ok "a start that cannot be paired is refused" "$([[ "$CH_CODE" != 0 ]] && echo 0 || echo 1)"
ok "and it is refused at once, not at the deadline (${CH_MS}ms)" \
  "$([[ "$CH_MS" -lt 5000 ]] && echo 0 || echo 1)"
ok "no channel is left behind" "$([[ "$(channels)" == "$before" ]] && echo 0 || echo 1)"

build=${GONGGA_BUILD:-$(git -C "$root" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)}
verdict=$([[ "$fails" == 0 ]] && echo PASS || echo FAIL)
printf '%s · %ss · %s failures · %s\n' "$verdict" "$(( $(date +%s) - began ))" "$fails" "$build"
exit $([[ "$fails" == 0 ]] && echo 0 || echo 1)
