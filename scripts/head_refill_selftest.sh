#!/usr/bin/env bash
# A re-stamped head reaches a loaded window after a handoff.
#
# The defect: handoff empties a session (head included); the server re-stamps the
# head on the next request, but a loaded client had marked the whole requested
# range held and never went back for the rows below its floor, so the head (tools,
# plan, ...) never drew again. The fix announces the re-stamped head as a written
# interval (`Transcript { first: Some(1), seq: 5 }`) and the client holds only rows
# it actually drew, fetching the owed interval as one Older page.
#
# Real server + fake LLM + a real `gongga-client` child showing the session. One
# turn stamps the head; a DB cut simulates the handoff (the skill's own effect:
# rows gone, context_tokens 0 -> generation bump); the note's turn re-stamps the
# head. The verdict is an `older` frame carrying the head rows (seq 1..5) AFTER the
# cut -- the page the fix fetches. One verdict line. `-v` prints each check.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/head-refill-home"
client_home="$work/head-refill-client"
log="$work/head-refill.log"
roster="$work/head-refill-roster.toml"
key=head-refill-selftest
verbose=${1:-}
start=$SECONDS

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || { echo "build first: mbx build" >&2; exit 1; }
[[ -x "$bin/gongga-client.exe" || -x "$bin/gongga-client" ]] || { echo "build first: mbx build -p gongga-client" >&2; exit 1; }
server_bin="$bin/gongga-server"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"
client_bin="$bin/gongga-client"; [[ -x "$client_bin" ]] || client_bin="$bin/gongga-client.exe"

free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { [[ -f "$home/server.lock" ]] && tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }

port=$(free_port); fake_port=$(free_port); fake_pid=""
# Only this home's own server: the pid in its lock, and the supervisor named in
# this run's log. Never a name or a command-line substring.
stop_servers() {
  local watch pid
  watch=$(grep -o 'supervisor=[0-9]*' "$log" 2>/dev/null | head -1 | cut -d= -f2)
  pid=$(child_pid)
  [[ -n "${watch:-}" ]] && alive "$watch" && taskkill //PID "$watch" //T //F >/dev/null 2>&1
  [[ -n "${pid:-}" ]] && alive "$pid" && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  sleep 0.5
}
cleanup() {
  stop_servers; stop_servers
  [[ -n "${fake_pid:-}" ]] && alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  rm -rf "$home" "$client_home" "$work/head-refill-drive.py" "$work/head-refill-fake.pid" 2>/dev/null || {
    sleep 2
    rm -rf "$home" "$client_home" 2>/dev/null || echo "note: $home is still held by a process" >&2
  }
}
trap cleanup EXIT

mkdir -p "$work"
stop_servers
rm -rf "$home" "$client_home"; mkdir -p "$home" "$client_home"
: > "$log"

# Content cloned from the main checkout's ship (a worktree's own ship is empty).
main_repo=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
main_repo=$(dirname "$main_repo")
[[ -d "$main_repo/ship" ]] && git clone -q --local "$main_repo/ship" "$home/ship" 2>/dev/null

cat > "$roster" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "HEAD_REFILL_SELFTEST_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:$fake_port/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Fake Flash"
  context = 100000
  modalities = ["text"]
TOML

export GONGGA_HOME="$home" GONGGA_CLIENT_HOME="$client_home"
export GONGGA_PROVIDERS_CATALOG="$roster" GONGGA_KEY="$key" HEAD_REFILL_SELFTEST_KEY=sk-fake

python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/head-refill-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >>"$work/head-refill-fake.log" 2>&1 &
for _ in $(seq 1 100); do [[ -s "$work/head-refill-fake.pid" ]] && break; sleep 0.1; done
fake_pid=$(tr -d '\r\n' < "$work/head-refill-fake.pid" 2>/dev/null)

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 300); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)
say "server pid=$server_pid fake pid=$fake_pid port=$port"

cli="$home/bin/gongga-cli"
bash "$cli" --home "$home" --server "127.0.0.1:$port" --key "$key" \
  '{"name":"set_session","args":{"id":"probe","model":"fake/flash-fake"}}' >/dev/null 2>&1

# The client shows probe, so its refill loop fetches the transcript (and the head).
cat > "$client_home/client.toml" <<'TOML'
[[render_session]]
name = "probe"
value = "true"
TOML

cat > "$work/head-refill-drive.py" <<'PY'
import json, os, queue, sqlite3, subprocess, sys, threading, time

root, work, home, client_home, addr, key, client_bin, fake_port = sys.argv[1:9]
checks = []

def ok(name, passed, detail=""):
    checks.append(bool(passed))
    print("  %s %s%s" % ("ok  " if passed else "FAIL", name, ("  " + detail) if detail and not passed else ""))

def cli(payload):
    return subprocess.run(["bash", os.path.join(home, "bin", "gongga-cli"),
                           "--home", home, "--server", addr, "--key", key, json.dumps(payload)],
                          capture_output=True, text=True, timeout=30)

class Client:
    def __init__(self):
        self.err = open(os.path.join(work, "head-refill-client.err"), "ab", buffering=0)
        self.p = subprocess.Popen(
            [client_bin, "--client_only", "--home", home, "--client-home", client_home,
             "--server", addr, "--key", key, "--parent", "990011"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err)
        self.q = queue.Queue(); self.frames = []
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for raw in self.p.stdout:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line: self.q.put(line)
        self.q.put(None)

    def submit(self, line):
        self.p.stdin.write((json.dumps({"intent": "submit", "line": line}) + "\n").encode())
        self.p.stdin.flush()

    def wait(self, pred, timeout=40, since=0):
        end = time.time() + timeout
        while True:
            for f in self.frames[since:]:
                if pred(f): return f
            left = end - time.time()
            if left <= 0: return None
            try: line = self.q.get(timeout=min(left, 0.25))
            except queue.Empty: continue
            if line is None: return None
            try: self.frames.append(json.loads(line))
            except ValueError: pass

    def drain(self, seconds=1.0):
        end = time.time() + seconds
        while time.time() < end:
            try: line = self.q.get(timeout=0.1)
            except queue.Empty: continue
            if line is None: break
            try: self.frames.append(json.loads(line))
            except ValueError: pass

    def close(self):
        if self.p.poll() is None:
            self.p.terminate()
            try: self.p.wait(timeout=5)
            except subprocess.TimeoutExpired: self.p.kill()
        self.err.close()

def sql(query, args=()):
    c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
    try: return c.execute(query, args).fetchone()[0]
    finally: c.close()

def ended(name):
    return lambda f: f.get("type") == "stopped" and f.get("session") == name

def head_older(f):
    if f.get("type") != "older" or f.get("window") != "probe":
        return False
    seqs = {r.get("seq") for r in f.get("rows", []) if isinstance(r, dict)}
    seqs |= {r.get("from") for r in f.get("rows", []) if isinstance(r, dict)}
    return {1, 2, 4, 5} <= {s for s in seqs if isinstance(s, int)}

ui = Client()
try:
    # First turn: the head is stamped (generation 0). Wait for it to end.
    since = len(ui.frames)
    cli({"name": "send_message", "args": {"session": "probe", "message": "first turn, say one word"}})
    end = ui.wait(ended("probe"), 60, since); ui.drain()
    ok("the first turn ran", end is not None)
    gen0 = sql("select context_generation from sessions where name='probe'")
    head0 = sql("select count(*) from messages where session_id=(select id from sessions where name='probe') and seq in (1,2,3,4,5)")
    ok("the first turn stamped a five-row head", head0 == 5, "head rows=%s" % head0)

    # The handoff's own effect on the store: rows gone, context_tokens 0 (the
    # session_context_reset trigger bumps generation). The skill keeps an in-flight
    # call; here the session is idle, so the cut takes everything.
    c = sqlite3.connect(f"{home}/gongga.db")
    sid = c.execute("select id from sessions where name='probe'").fetchone()[0]
    for (seq,) in c.execute("select seq from messages where session_id=? order by seq desc", (sid,)).fetchall():
        c.execute("delete from messages where session_id=? and seq=?", (sid, seq))
    c.execute("update sessions set context_tokens=0 where id=?", (sid,))
    c.commit(); c.close()
    gen1 = sql("select context_generation from sessions where name='probe'")
    ok("the cut bumped context_generation", gen1 > gen0, "gen %s -> %s" % (gen0, gen1))

    # The note's turn re-stamps the head and announces the interval. The loaded
    # window must fetch seq 1..5 as an Older page -- the head it had lost.
    since = len(ui.frames)
    cli({"name": "send_message", "args": {"session": "probe", "message": "continuation note, say one word"}})
    page = ui.wait(head_older, 60, since)
    end = ui.wait(ended("probe"), 60, since); ui.drain()
    ok("the continuation turn ran", end is not None)
    ok("a loaded window fetched the re-stamped head as an Older page (seq 1,2,4,5)",
       page is not None, "frames=%s" % json.dumps([f.get("type") for f in ui.frames[since:]]))
    head1 = sql("select count(*) from messages where session_id=(select id from sessions where name='probe') and seq in (1,2,3,4,5)")
    ok("the head is whole again in the store", head1 == 5, "head rows=%s" % head1)
finally:
    ui.close()

sys.exit(0 if checks and all(checks) else 1)
PY

python "$work/head-refill-drive.py" "$root" "$work" "$home" "$client_home" "127.0.0.1:$port" "$key" "$client_bin" "$fake_port"
result=$?
[[ "$verbose" == "-v" ]] && tail -20 "$work/head-refill-client.err" 2>/dev/null

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $result -eq 0 ]]; then
  echo "PASS · $((SECONDS-start))s · head-refill · $sha"
else
  echo "FAIL · head-refill · $sha"
fi
exit $result
