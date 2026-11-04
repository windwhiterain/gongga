#!/usr/bin/env bash
# The bundle, on real processes: a build that replaces the one before it stops
# the server of the old build, and a window that was told not to start a server
# must not lose the one it was told to use.
#
# `--client_only` exists so that a server upgrade does not break the client, and
# the retire is where that promise is kept: it stops the old build's server on
# the way in, which is exactly the server such a window is waiting for.
#
# One line of verdict at the end; `-v` prints what each scenario saw.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bundle="$root/target/debug/gongga.exe"
home="$root/.dev/selftest/bundle-home"
log="$home/stdout.log"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { # name, condition-result
  if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi
}

# The payload the bundle carries is only there with the feature on, and a plain
# `cargo build` leaves it out.
[[ -x "$bundle" ]] || {
  echo "build first: cargo build -p gongga-bundle --features payload" >&2; exit 1; }

# The kernel picks the port: two agents on one machine must not fight over one.
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}

# Every process serving THIS home, whatever the run managed to remember: a
# scenario that fails before its pid is recorded would otherwise leave a server
# behind, and the next run would mistake it for its own. Identity is the home in
# the command line, never the image name -- other servers share that.
serving_home() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-server.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

# The window of this home, by the home in its command line: it is a window, and
# it does not end by itself.
window_pids() {
  local win; win=$(cygpath -w "$home" 2>/dev/null || echo "$home")
  local posix; posix=$(cygpath -m "$home" 2>/dev/null || echo "$home")
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='gongga-tui.exe'\" | Where-Object { \$_.CommandLine -like '*--home *$posix*' -or \$_.CommandLine -like '*--home *$win*' -or \$_.CommandLine -like '*--home *$home*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | tr '\n' ' '
}

# The marks of a server that was asked to stop: a cancel over the protocol, a
# kill by the retire, or the window's own death.
stopped() { grep -qE "the server is stopping|the server stopped without saying why|command ran command=cancel" "$1" 2>/dev/null; }

# One scenario: $1 = yes to hand the window `--client_only`, $2 = the word the
# older payload's server's fate should have.
scenario() {
  local client_only="$1" expect="$2"
  local port; port=$(free_port)
  rm -rf "$home"
  mkdir -p "$home"

  # The build lays its payload down; a copy of it stands for an older build.
  GONGGA_HOME="$home" "$bundle" --version >/dev/null 2>&1
  local payload; payload=$(ls -d "$home"/payload/*/ 2>/dev/null | head -1)
  [[ -n "$payload" ]] || {
    echo "the bundle placed no payload: build it with cargo build -p gongga-bundle --features payload" >&2
    return 1
  }
  cp -r "$payload" "$home/payload/older"

  # A server of that older build, running on its own port.
  "$home/payload/older/gongga-server.exe" --home "$home" --bind "127.0.0.1:$port" \
      --log info >"$log" 2>&1 &
  for _ in $(seq 1 300); do
    grep -q "listening (UDP) local=127.0.0.1:$port" "$log" 2>/dev/null && break
    sleep 0.2
  done
  grep -q "listening (UDP) local=127.0.0.1:$port" "$log" || {
    echo "the server never listened" >&2
    return 1
  }

  # The build that replaces it runs. The window is a window: it does not end by
  # itself, and the retire has already run by the time it has its first frame.
  local flag=""
  [[ "$client_only" == yes ]] && flag="--client_only"
  GONGGA_HOME="$home" "$bundle" tui $flag --home "$home" --client-home "$home/client" \
      --server "127.0.0.1:$port" >"$home/bundle.log" 2>&1 &
  sleep 8

  local alive=yes
  stopped "$log" && alive=no
  say "the older payload's server is $alive (wanted $expect)"

  # Everything this scenario started, and nothing else: the window first, so the
  # server it took as its owner is not stopped by its death but by this list.
  for pid in $(window_pids); do taskkill //F //T //PID "$pid" >/dev/null 2>&1; done
  for pid in $(serving_home); do taskkill //F //T //PID "$pid" >/dev/null 2>&1; done
  [[ "$alive" == "$expect" ]]
}

scenario yes yes; ok "a client-only window keeps the server it was told to use" $?
scenario no no; ok "any other run replaces that server" $?
rm -rf "$home"

if [[ $fails == 0 ]]; then
  echo "PASS  the bundle stops a server only when the program about to run will replace it"
else
  echo "FAIL  $fails scenario(s) failed"
fi
exit $((fails > 0))
