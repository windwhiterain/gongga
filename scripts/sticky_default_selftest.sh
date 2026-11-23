#!/usr/bin/env bash
# A defaulted struct field is never sticky-filled. SPEC: `live_compact: LiveCompact
# = default()` and each LiveCompact field `= default()`, so a `new_session` that
# omits `config` must not inherit a `live_compact` a window holds as sticky. The
# fill is the window's, so this drives a real client's submit pipe, not the one-shot
# cli (which keeps no state). One server, one client, one verdict.
set -uo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/sticky-default-selftest"
home="$work/home"
log="$work/server.log"
roster="$work/roster.toml"

[[ -x "$bin/gongga-server.exe" && -x "$bin/gongga-client.exe" ]] || { echo "build first: mbx build" >&2; exit 1; }
server_bin="$bin/gongga-server.exe"; client_bin="$bin/gongga-client.exe"; cli_bin="$home/bin/gongga-cli"

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
free_port() { python -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }
wait_listening() { for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done; return 1; }

port=$(free_port)
key=$(python -c 'import uuid;print(uuid.uuid4().hex)')
# The watchdog restarts a killed child in 200ms: kill the supervisor first, named
# by this run's own log, then the child whose pid is in this home's lock.
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
  rm -rf "$home" "$work/ui" "$work/client.err" 2>/dev/null || { sleep 2; rm -rf "$home" "$work/ui" "$work/client.err" 2>/dev/null || echo "note: $home is still held" >&2; }
}
trap cleanup EXIT

mkdir -p "$work"; stop_servers
rm -rf "$home" "$work/ui"; mkdir -p "$home"; : > "$log"
export GONGGA_HOME="$home"
cat > "$roster" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "STICKY_DEFAULT_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "http://127.0.0.1:1/v1"

  [[provider.model]]
  id = "flash-fake"
  name = "Fake Flash"
  context = 100000
  modalities = ["text"]
TOML
export GONGGA_PROVIDERS_CATALOG="$roster"
export STICKY_DEFAULT_KEY=sk-fake

env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_HOME -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_KEY -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  -u GONGGA_API_KEY_ENV GONGGA_KEY="$key" \
  "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)

w_client=$(cygpath -m "$client_bin"); w_cli=$(cygpath -m "$cli_bin")
w_home=$(cygpath -m "$home"); w_work=$(cygpath -m "$work")
python - "$w_client" "$w_cli" "$w_home" "$w_work" "127.0.0.1:$port" "$key" "$server_pid" <<'PY'
import json, os, queue, sqlite3, subprocess, sys, threading, time

client_bin, cli_bin, home, work, addr, key, server_pid = sys.argv[1:8]
checks = []
def ok(name, passed, detail=""):
    checks.append(bool(passed))
    print("  %s%s%s" % ("ok   " if passed else "FAIL ", name, (" -- " + detail) if detail and not passed else ""))

def cli(*args):
    return subprocess.run(["bash", cli_bin, "--home", home, "--server", addr, "--key", key, *args],
                          capture_output=True, text=True, timeout=20)

def probe(name, tries=40):
    for _ in range(tries):
        if cli(json.dumps({"name": "get_messages", "args": {"session": name}})).returncode == 0:
            return True
        time.sleep(0.2)
    return False

class Peer:
    def __init__(self):
        self.dir = os.path.join(work, "ui"); os.makedirs(self.dir, exist_ok=True)
        self.err = open(os.path.join(work, "client.err"), "ab", buffering=0)
        argv = [client_bin, "--client_only", "--home", self.dir, "--client-home", self.dir,
                "--server", addr, "--key", key, "--parent", "990001"]
        self.p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err)
        self.q = queue.Queue(); self.frames = []
        threading.Thread(target=self._read, daemon=True).start()
    def _read(self):
        for raw in self.p.stdout:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line: self.q.put(line)
        self.q.put(None)
    def submit(self, line):
        self.p.stdin.write((json.dumps({"intent": "submit", "line": line}) + "\n").encode()); self.p.stdin.flush()
    def wait_server(self, timeout=25):
        end = time.time() + timeout
        while time.time() < end:
            try: line = self.q.get(timeout=0.25)
            except queue.Empty: continue
            if line is None: return None
            try: f = json.loads(line)
            except ValueError: continue
            self.frames.append(f)
            if f.get("type") == "server": return f
        return None
    def close(self):
        if self.p.poll() is None:
            self.p.terminate()
            try: self.p.wait(timeout=5)
            except subprocess.TimeoutExpired: self.p.kill(); self.p.wait(timeout=5)
        self.err.close()

def interval(name):
    c = sqlite3.connect(f"file:{home}/gongga.db?mode=ro", uri=True)
    row = c.execute("select live_compact_interval_tokens from sessions where name=?", (name,)).fetchone()
    c.close()
    return row[0] if row else None

a = Peer()
try:
    ok("client dialled the server", a.wait_server() is not None)
    # s1 states a non-default live_compact interval: it is kept, and the window writes it back.
    a.submit('/set_session{"id":"s1","config":{"model_config":{"model":"fake/flash-fake","live_compact":{"interval_tokens":1234}}}}')
    ok("set_session s1 reached the server", probe("s1"))
    ok("a stated live_compact interval is kept", interval("s1") == 1234, f"interval={interval('s1')!r}")
    time.sleep(0.5)
    # s2 omits config: the defaulted live_compact must not be filled from the window's entry.
    a.submit('/new_session{"id":"s2"}')
    ok("new_session s2 reached the server", probe("s2"))
    v = interval("s2")
    ok("new_session did not inherit the sticky live_compact interval", v != 1234, f"interval={v!r}")
finally:
    a.close()

print("PASS" if all(checks) else "FAIL")
sys.exit(0 if all(checks) else 1)
PY
rc=$?
if [[ $rc -ne 0 ]]; then echo "FAIL · build $(git -C "$root" rev-parse --short HEAD) · log $log"; exit 1; fi
echo "PASS · build $(git -C "$root" rev-parse --short HEAD)"