#!/usr/bin/env bash
# SPEC: every tool but `wait` may run `background`, which answers at once with a
# `job_id` that `check_job` collects -- and a job that outlives its answer is said
# lost by the next start rather than waited for for ever.
#
# One server lifetime, one table of scenarios, one verdict. Every wait here is a
# dial or a log line, never a sleep that hopes.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/job-home"
log="$work/job.log"
began=$SECONDS

fails=0
ok() { if [[ "$2" == 0 ]]; then printf '  ok   %s\n' "$1"; else printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); fi; }

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
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

port=$(free_port)
cleanup() {
  # The pid from the home's own lock, never `$!` and never a name: a test server
  # is not distinguishable from a real one by anything but its home.
  local pid; pid=$(cat "$home/server.lock" 2>/dev/null)
  alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  # It writes on the way out, so the home is gone only once the process is.
  for _ in $(seq 1 100); do
    alive "$pid" || break
    sleep 0.2
  done
  rm -rf "$home" || echo "note: $home is still held by a process" >&2
}
trap cleanup EXIT

mkdir -p "$work"; rm -rf "$home"; mkdir -p "$home"
cp -r "$HOME/.gongga/ship" "$home/ship" 2>/dev/null
: > "$log"

start() {
  env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
      -u GONGGA_API_KEY_ENV "$bin/gongga-server.exe" \
      --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
  for _ in $(seq 1 300); do
    grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0
    sleep 0.2
  done
  return 1
}

cli=("$bin/gongga-cli.exe" --home "$home" --server "127.0.0.1:$port")
# The CLI echoes its own command frame, so the answer is the last line: grepping
# the whole stream would match the arguments the caller just typed.
run() { "${cli[@]}" "$@" 2>&1 | tail -n 1; }
job_of() { sed -n 's/.* job \([0-9][0-9]*\)[.:].*/\1/p' <<<"$1"; }

start || { echo "FAIL · job · the server never listened"; sed -n '1,20p' "$log"; exit 1; }
run set_session probe --cwd "$work" >/dev/null 2>&1
run set_session other --cwd "$work" >/dev/null 2>&1

# ---- a tool with no background of its own ----------------------------------

t0=$SECONDS
said=$(run exec probe '["bash","-c","sleep 3; echo slow-done"]' --background true)
took=$((SECONDS - t0))
job=$(job_of "$said")
[[ -n "$job" ]]
ok "exec --background answers with a job id (job $job)" $?

[[ $took -lt 3 ]]
ok "it answered before the 3s of work it started (${took}s)" $?

grep -q "slow-done" <<<"$said" && echoed=1 || echoed=0
[[ "$echoed" == 0 ]]
ok "the answer is a receipt, not the job's output" $?

said=$(run check_job other --job_id "$job")
grep -q "no job $job" <<<"$said"
ok "another session's job is not this session's to read" $?

said=$(run check_job probe --job_id "$job" --wait_ms 30000)
grep -q "slow-done" <<<"$said"
ok "check_job collects what the job printed" $?
grep -q "finished successfully" <<<"$said"
ok "check_job says how it ended" $?

said=$(run check_job probe --job_id "$job")
grep -q "already been collected" <<<"$said"
ok "reading an answer clears it, as check_bash does" $?

said=$(run check_job probe --job_id 999999)
grep -q "no job 999999" <<<"$said"
ok "an id this session never had is not found" $?

# ---- SPEC: a foreground job over its timeout becomes background -----------

t0=$SECONDS
said=$(run exec probe '["bash","-c","sleep 4; echo late-answer"]' --timeout_ms 1000)
took=$((SECONDS - t0))
promoted=$(job_of "$said")
[[ -n "$promoted" ]]
ok "a call past its timeout became background job $promoted" $?
[[ $took -lt 4 ]]
ok "the turn stopped waiting at its clock (${took}s)" $?
grep -q "late-answer" <<<"$said" && early=1 || early=0
[[ "$early" == 0 ]]
ok "the receipt did not wait for the work" $?

said=$(run check_job probe --job_id "$promoted" --wait_ms 30000)
grep -q "late-answer" <<<"$said"
ok "the work it became still answers, through check_job" $?

# ---- del_job stops the work, not just the row ------------------------------

marker="$work/deljob-marker"
rm -f "$marker"
said=$(run exec probe "[\"bash\",\"-c\",\"sleep 6; echo late > $marker\"]" --background true)
doomed=$(job_of "$said")
[[ -n "$doomed" ]]
ok "a job to delete was started (job $doomed)" $?

said=$(run del_job probe --job_id "$doomed")
grep -q "and stopped it" <<<"$said"
ok "del_job stops it" $?

said=$(run check_job probe --job_id "$doomed")
grep -q "no job $doomed" <<<"$said"
ok "a deleted job is not collectable" $?

sleep 8
[[ -f "$marker" ]] && wrote=1 || wrote=0
[[ "$wrote" == 0 ]]
ok "the work it was running never finished on its own" $?

# A detached job is a process: deleting it has to end that, not a task.
marker2="$work/deljob-worker"
rm -f "$marker2"
said=$(run exec probe "[\"bash\",\"-c\",\"sleep 6; echo late > $marker2\"]" --detached true)
doomed2=$(job_of "$said")
said=$(run del_job probe --job_id "$doomed2")
grep -q "and stopped it" <<<"$said"
ok "del_job stops a detached worker" $?
sleep 8
[[ -f "$marker2" ]] && wrote2=1 || wrote2=0
[[ "$wrote2" == 0 ]]
ok "the worker it owned never finished on its own" $?

# ---- a tool that has a background of its own keeps it ----------------------

said=$(run bash probe --command "echo native-slow" --background true)
grep -q "started background job" <<<"$said"
ok "bash --background is still the shell's own job" $?
said=$(run check_bash probe --wait_ms 10000)
grep -q "native-slow" <<<"$said"
ok "check_bash still reads it" $?

# ---- the words the tunnel does not offer -----------------------------------

# `wait` is the one tool the tunnel leaves alone, so the word is not in its
# schema at all: the caller is told what it does take.
said=$(run wait probe --background true --timeout_ms 1000)
grep -q "wait has no argument --background" <<<"$said"
ok "wait does not take background, and says so" $?

said=$(run exec probe '["bash","-c","echo both"]' --background true --detached true)
grep -q "pick one" <<<"$said"
ok "a call that says both ways is refused" $?

# ---- what a stopped run owed ----------------------------------------------

said=$(run exec probe '["bash","-c","sleep 60; echo never"]' --background true)
lost=$(job_of "$said")
[[ -n "$lost" ]]
ok "a long background job is running when the server is killed" $?

# SPEC: a detached job is a process of its own, so the kill is not its end.
said=$(run exec probe '["bash","-c","sleep 20; echo detach-survives"]' --detached true)
kept=$(job_of "$said")
[[ -n "$kept" ]]
ok "a detached call started as job $kept" $?

kept_pid=$(python - "$home" "$kept" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("select pid from tool_jobs where id = ?", (int(sys.argv[2]),)).fetchone()
print((row and row[0]) or 0)
INNER
)
[[ "${kept_pid:-0}" -gt 0 ]]
ok "the row names the process that owns it (pid $kept_pid)" $?

# An old home has no `tool_jobs`: the row a `detached` call left is the row that
# must survive, or a session is never told what it lost.
python - "$home" <<'INNER'
import sqlite3, sys, time
c = sqlite3.connect(f"{sys.argv[1]}/gongga.db")
c.execute("""CREATE TABLE IF NOT EXISTS detached_calls (
    id INTEGER PRIMARY KEY, session TEXT NOT NULL, call_id TEXT NOT NULL,
    tool TEXT NOT NULL, description TEXT, started_at INTEGER NOT NULL)""")
c.execute("INSERT INTO detached_calls (session, call_id, tool, description, started_at)"
          " VALUES ('probe', 'old-call', 'exec', 'from an older build', ?)",
          (int(time.time() * 1000),))
c.commit()
INNER
ok "an older home's detached_calls row is planted" $?

# A restart, the way one happens: `cancel` ends the server and leaves what it started
# detached alone. A tree kill would take the worker too, which no restart does.
run cancel >/dev/null 2>&1
for _ in $(seq 1 150); do dial_ready "$port" || break; sleep 0.2; done
sleep 1
: > "$log"
start || { echo "FAIL · job · the second start never listened"; sed -n '1,20p' "$log"; exit 1; }

grep -q "reported jobs a stopped server left owed" "$log"
ok "the next start says what the last run owed" $?

said=$(run check_job probe --job_id "$lost" --wait_ms 5000)
grep -q "never reported" <<<"$said"
ok "the job of the server's own died with it" $?

# Whatever the restart found -- still running, or already answered -- it is the same
# process: the row the next start reads names the pid the last one recorded.
same_pid=$(python - "$home" "$kept" "$kept_pid" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
row = c.execute("select pid from tool_jobs where id = ?", (int(sys.argv[2]),)).fetchone()
print(1 if row and row[0] == int(sys.argv[3]) else 0)
INNER
)
[[ "$same_pid" == 1 ]]
ok "the restart picked up the same worker, not a re-run" $?

said=$(run check_job probe --job_id "$kept" --wait_ms 30000)
grep -q "already been collected" <<<"$said"
ok "the adopted job finished, and its answer went out as a notice" $?

python - "$home" "$kept" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
status, output = c.execute("select status, output from tool_jobs where id = ?",
                           (int(sys.argv[2]),)).fetchone()
assert status == "ok", status
assert "detach-survives" in (output or ""), output
told = c.execute("select payload from pending_tasks where session = 'probe'").fetchall()
assert any("detach-survives" in (p or "") for (p,) in told), told
INNER
ok "what the worker answered is in its row, and the session was told" $?

python - "$home" "$lost" <<'INNER'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}/gongga.db?mode=ro", uri=True)
rows = dict(c.execute("select id, status from tool_jobs").fetchall())
assert rows.get(int(sys.argv[2])) == "lost", rows
carried = c.execute("select mode, status from tool_jobs where call_id = 'old-call'"
                    ).fetchall()
assert carried == [("detached", "lost")], carried
gone = c.execute("select name from sqlite_master where name = 'detached_calls'"
                 ).fetchall()
assert gone == [], gone
INNER
ok "the old table's row became a job, and the old table is gone" $?

elapsed=$((SECONDS - began))
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails == 0 ]]; then printf 'PASS · job · %ss · %s\n' "$elapsed" "$sha"
else printf 'FAIL · job · %d scenario(s) · %ss · %s\n' "$fails" "$elapsed" "$sha"; exit 1; fi
