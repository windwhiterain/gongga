#!/usr/bin/env bash
# The handover's one destructive verb: `clean` ends the successors nobody will swap,
# and must never end anything a `taskkill /T` from the server would take with it.
#
# Three shapes, measured on this machine rather than imagined -- and the measurement
# is why the third one is what it is:
#
#   1. the ordinary Windows handover. `exec` of a native exe leaves the launcher bash
#      *dead*, so `launch.pid` names a pid that is gone and the server records it only
#      as a stale parent. Nothing to kill, and nothing may be killed.
#   2. a launcher that kept the server's pid, which is what unix `exec` leaves. Record
#      it as the server's own pid and `clean` must keep it -- that is the shape that
#      made `clean` dangerous before this test existed.
#   3. a superseded waiter that serves nothing. That one is ended.
#
# A walk up the process tree is *not* tested, because it cannot be answered here: MSYS
# records `ParentProcessId` values that are not the visible processes' Windows pids.
# The code says so too, and only claims the two facts it can check.
#
# Every process is a stand-in started here, and every pid is read from the process
# table by program name plus a marker in its arguments -- never `$!`, which is an
# MSYS number `taskkill` does not take.
#
# One line of verdict at the end; `-v` keeps the scratch and shows each clean.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
evolve="$root/.agents/skills/evolve/evolve.sh"
verbose=${1:-}
started=$(date +%s)

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }
fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }
[[ -f "$evolve" ]] || { echo "no $evolve" >&2; exit 1; }

tag="evolve-selftest-$$"
run="$root/.dev/$tag"
home="$run/home"
mkdir -p "$run" "$home"

# The stand-in server: `sleep` under a name this run owns, so the process table can
# be asked for it and nothing else on the machine answers.
fake="$run/gongga-fake-server-$tag.exe"
cp "$(command -v sleep).exe" "$fake" 2>/dev/null || cp "$(command -v sleep)" "$fake"

# A pid from the process table, by program name and a marker in its arguments.
#
# The name is not decoration: this function's own `powershell` carries the marker in
# its command line, so filtering on the marker alone finds the query, not the process
# (measured: it reported an exited powershell as the launcher).
win_pid_of() { # name marker
  powershell -NoProfile -Command \
    "Get-CimInstance Win32_Process -Filter \"Name='$1'\" | Where-Object { \$_.CommandLine -like '*$2*' } | ForEach-Object { \$_.ProcessId }" \
    2>/dev/null | tr -d '\r' | head -n 1
}
parent_of_win() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" \
    2>/dev/null | tr -d '\r' | grep -E '^[0-9]+$' | head -n 1
}
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
wait_pid() { # name marker
  local pid=""
  for _ in $(seq 1 60); do
    pid=$(win_pid_of "$1" "$2")
    [[ -n "$pid" ]] && { printf '%s' "$pid"; return 0; }
    sleep 0.2
  done
  return 1
}

pids=()
cleanup() {
  for pid in "${pids[@]}"; do alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1; done
  sleep 1
  [[ "$verbose" == "-v" ]] || rm -rf "$run"
}
trap cleanup EXIT

clean() { GONGGA_EVOLVE_RUN="$run" GONGGA_HOME="$home" bash "$evolve" clean 2>&1; }

# ---- shape 1: the launcher is dead, and its pid is only the server's stale parent
cat > "$run/dead-$tag.sh" <<DEAD
#!/usr/bin/env bash
exec "$fake" 300
DEAD
chmod +x "$run/dead-$tag.sh"
bash "$run/dead-$tag.sh" >/dev/null 2>&1 </dev/null &
sleep 2
server_pid=$(win_pid_of "gongga-fake-server-$tag.exe" "gongga-fake-server-$tag.exe")
[[ -n "${server_pid:-}" ]]
ok "a stand-in server is running (pid ${server_pid:-none})" $?
pids+=("$server_pid")
stale=$(parent_of_win "$server_pid")
[[ -n "$stale" ]] && ! alive "$stale"
ok "its launcher is dead and recorded only as a stale parent (pid ${stale:-none})" $?
echo "$stale" > "$run/evolve-20200101-000000-launch.pid"
echo "$server_pid" > "$home/server.lock"
echo "20200101-999999" > "$run/evolve-current"

out=$(clean); say "$out"
grep -q "0 killed" <<<"$out"
ok "a dead launcher is no candidate" $?
alive "$server_pid"
ok "and the server it started is untouched" $?

# Shape 1 has said what it had to say; end its stand-in so shape 2 has one server to
# find by name (the query takes the first match, and an older one is a different test).
taskkill //PID "$server_pid" //T //F >/dev/null 2>&1
sleep 1
! alive "$server_pid"
ok "the stand-in is ended by this script, not by clean" $?

# ---- shape 2a: the launcher kept the server's pid (unix `exec`)
cat > "$run/kept-$tag.sh" <<KEPT
#!/usr/bin/env bash
sleep 300
KEPT
chmod +x "$run/kept-$tag.sh"
bash "$run/kept-$tag.sh" >/dev/null 2>&1 </dev/null &
kept_pid=$(wait_pid bash.exe "kept-$tag.sh")
[[ -n "${kept_pid:-}" ]]
ok "a stand-in server is running (pid ${kept_pid:-none})" $?
pids+=("$kept_pid")
echo "$kept_pid" > "$home/server.lock"
echo "$kept_pid" > "$run/evolve-20200101-000001-launch.pid"
echo "20200101-999999" > "$run/evolve-current"

out=$(clean); say "$out"
grep -q "keep    20200101-000001 (pid $kept_pid is the server or the process it hangs from)" <<<"$out"
ok "clean keeps the launcher that holds the server's pid" $?
alive "$kept_pid"; ok "that process is still running" $?

# ---- shape 2b: a live parent, which is the branch `p = server_pid` cannot cover.
#
# Both ends native, because that is the only parent link this platform records
# truthfully: a `powershell` that starts the stand-in and waits for it is the same
# shape a bundle leaves when it spawns the program it carries -- and `taskkill /T` on
# it takes the server with it.
cat > "$run/parent-$tag.ps1" <<'PS1'
param([string]$Exe)
$child = Start-Process -FilePath $Exe -ArgumentList 300 -PassThru
Wait-Process -Id $child.Id
PS1
powershell -NoProfile -ExecutionPolicy Bypass -File "$run/parent-$tag.ps1" -Exe "$fake" >/dev/null 2>&1 </dev/null &
parent_pid=$(wait_pid powershell.exe "parent-$tag.ps1")
[[ -n "${parent_pid:-}" ]]
ok "a launcher is alive (pid ${parent_pid:-none})" $?
pids+=("$parent_pid")
child_pid=$(win_pid_of "gongga-fake-server-$tag.exe" "gongga-fake-server-$tag.exe")
[[ -n "${child_pid:-}" && "$child_pid" != "$parent_pid" ]]
ok "with a server below it (pid ${child_pid:-none})" $?
pids+=("$child_pid")
[[ "$(parent_of_win "$child_pid")" == "$parent_pid" ]]
ok "and it is that server's parent" $?
echo "$child_pid" > "$home/server.lock"
echo "$parent_pid" > "$run/evolve-20200101-000001-launch.pid"

out=$(clean); say "$out"
grep -q "keep    20200101-000001 (pid $parent_pid is the server or the process it hangs from)" <<<"$out"
ok "clean keeps a launcher the server hangs from" $?
alive "$parent_pid"; ok "that launcher is still running" $?
alive "$child_pid"; ok "and so is the server below it" $?

# ---- shape 3: a superseded waiter, serving nothing
cat > "$run/waiter-$tag.sh" <<WAIT
#!/usr/bin/env bash
sleep 300
WAIT
chmod +x "$run/waiter-$tag.sh"
bash "$run/waiter-$tag.sh" >/dev/null 2>&1 </dev/null &
waiter_pid=$(wait_pid bash.exe "waiter-$tag.sh")
[[ -n "${waiter_pid:-}" ]]
ok "a superseded waiter is running (pid ${waiter_pid:-none})" $?
pids+=("$waiter_pid")
echo "$waiter_pid" > "$run/evolve-20200101-000002-launch.pid"

out=$(clean); say "$out"
grep -q "killed  20200101-000002" <<<"$out"
ok "clean ends a waiter nobody will swap" $?
sleep 1
! alive "$waiter_pid"
ok "that waiter is gone" $?
alive "$child_pid"
ok "the server is still running after it" $?
grep -q "1 killed, 0 left running, 1 kept" <<<"$out"
ok "and the summary counts what it kept" $?

elapsed=$(( $(date +%s) - started ))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then
  printf 'PASS · %ss · %s\n' "$elapsed" "$sha"
else
  printf 'FAIL · %s check(s) red · %ss · %s\n' "$fails" "$elapsed" "$sha"
fi
[[ $fails == 0 ]]
