#!/usr/bin/env bash
# The turn-end notice is the client's own act, and only a session this client owns
# raises one. A real server, a real `gongga-client` child with NO front-end attached,
# and the fake LLM: the toast must come from the client process itself, and a session
# nobody marked must raise nothing. The toast child is intercepted (its payload read,
# then killed) so a verification pops no notification on the operator's desktop.
#
# One verdict line at the end. `-v` prints each check.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/notice-home"
client_home="$work/notice-client"
log="$work/notice.log"
roster="$work/notice-roster.toml"
verbose=${1:-}

say() { [[ "$verbose" == "-v" ]] && printf '%s\n' "$*"; return 0; }

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: mbx build" >&2; exit 1; }
server_bin="$bin/gongga-server"; [[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"
client_bin="$bin/gongga-client"; [[ -x "$client_bin" ]] || client_bin="$bin/gongga-client.exe"

free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
# Whose server this is: the home is the identity. Every server (and its env
# wrapper) whose command line names THIS unique temp home is ours; nothing else is
# touched. The pattern is asserted non-empty first -- an empty pattern matches all.
# Ancestors first, so a supervisor cannot restart a worker.
serving_tree() {
  local home_win; home_win=$(cygpath -m "$home")
  [[ -n "$home_win" ]] || return 0
  python - "$home_win" <<'PY'
import subprocess, sys
h = sys.argv[1]
if not h.strip():
    sys.exit(0)
out = subprocess.run(
    ["powershell", "-NoProfile", "-Command",
     "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId)|$($_.ParentProcessId)|$($_.Name)|$($_.CommandLine)\" }"],
    capture_output=True, text=True).stdout
procs = {}
for line in out.splitlines():
    parts = line.strip().split("|", 3)
    if len(parts) == 4 and parts[0].isdigit():
        procs[int(parts[0])] = (int(parts[1]) if parts[1].isdigit() else 0, parts[2], parts[3])
mine = {p for p, (par, name, cmd) in procs.items()
        if h in cmd and name in ("gongga-server.exe", "env.exe")}
def depth(p):
    d, cur = 0, p
    while cur in procs and procs[cur][0] in mine and d < 10:
        cur = procs[cur][0]; d += 1
    return d
for p in sorted(mine, key=depth):
    sys.stdout.buffer.write((str(p) + "\n").encode())
PY
}
wait_listening() {
  for _ in $(seq 1 300); do grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0; sleep 0.2; done
  return 1
}

port=$(free_port); fake_port=$(free_port); fake_pid=""; server_pid=""
cleanup() {
  for pid in $(serving_tree); do alive "$pid" && taskkill //PID "$pid" //F >/dev/null 2>&1; done
  alive "$fake_pid" && taskkill //PID "$fake_pid" //F >/dev/null 2>&1
  sleep 2
  rm -rf "$home" "$client_home" 2>/dev/null || {
    sleep 2
    rm -rf "$home" "$client_home" 2>/dev/null || echo "note: $home is still held by a process" >&2
  }
}
trap cleanup EXIT

mkdir -p "$work"
rm -rf "$home" "$client_home"; mkdir -p "$home" "$client_home"
# The content checkout, cloned from this repository's own ship: a start needs no
# network, and the clone is independent (no gitfile pointing at another repo).
main_repo=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
main_repo=$(dirname "$main_repo")
if [[ -d "$main_repo/ship" ]]; then
  git clone -q --local "$main_repo/ship" "$home/ship" 2>/dev/null
fi
: > "$log"

python -c "import os,runpy,sys;open(sys.argv[1],'w').write(str(os.getpid()));sys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')" \
  "$work/notice-fake.pid" "$root/scripts/fake_llm.py" "$fake_port" >"$work/notice-fake.log" 2>&1 &
for _ in $(seq 1 50); do [[ -s "$work/notice-fake.pid" ]] && break; sleep 0.2; done
fake_pid=$(cat "$work/notice-fake.pid" 2>/dev/null)

cat > "$roster" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "NOTICE_SELFTEST_KEY"
cache_ttl_tiers = { "5m" = 300, "1h" = 3600 }

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

# The file the old default left: one session drawn, two only looked at, and no
# client_session section at all.
cat > "$client_home/client.toml" <<'TOML'
[[render_session]]
name = "own"
value = "true"

[[render_session]]
name = "sub/agent"
value = "false"

[[render_session]]
name = "@monitor/disk"
value = "false"
TOML

export GONGGA_HOME="$home" GONGGA_CLIENT_HOME="$client_home"
export GONGGA_PROVIDERS_CATALOG="$roster" GONGGA_KEY="notice-selftest" NOTICE_SELFTEST_KEY=sk-fake
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV \
  -u GONGGA_SERVER -u GONGGA_BIND -u GONGGA_SESSION -u GONGGA_CLI -u GONGGA_REMOTE -u GONGGA_REMOTE_CWD \
  "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 300); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)

# Sessions the server holds, none created in a window.
cli="$home/bin/gongga-cli"
for name in own sub/agent @monitor/disk; do
  bash "$cli" --home "$home" --server "127.0.0.1:$port" \
    "{\"name\":\"set_session\",\"args\":{\"id\":\"$name\",\"model\":\"fake/flash-fake\"}}" >/dev/null 2>&1
done

cat > "$work/notice-drive.py" <<'PY'
import base64, json, os, queue, re, subprocess, sys, threading, time

root, work, home, client_home, addr, key, client_bin = sys.argv[1:8]
checks = []

def ok(name, passed, detail=""):
    checks.append((name, bool(passed)))
    print("  %s %s%s" % ("ok  " if passed else "FAIL", name, ("  " + detail) if detail else ""))

def read(path):
    try:
        with open(path, encoding="utf-8") as f: return f.read()
    except OSError: return ""

def marks():
    return {n: v == "true" for n, v in re.findall(
        r'\[\[client_session\]\]\nname = "(.*?)"\nvalue = "(.*?)"',
        read(os.path.join(client_home, "client.toml")))}

def wait_for(pred, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        if pred(): return True
        time.sleep(0.2)
    return False

class Client:
    def __init__(self):
        self.err = open(os.path.join(work, "notice-client.err"), "ab", buffering=0)
        self.p = subprocess.Popen(
            [client_bin, "--client_only", "--home", home, "--client-home", client_home,
             "--server", addr, "--key", key, "--parent", "990009"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err)
        self.q = queue.Queue(); self.frames = []; self.toasts = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._watch_toasts, daemon=True).start()

    def _read(self):
        for raw in self.p.stdout:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line: self.q.put(line)
        self.q.put(None)

    def _watch_toasts(self):
        # The notice is a powershell child of this client. Read its payload, then
        # kill it: a verification observes the notice without popping a desktop toast.
        while self.p.poll() is None:
            script = ("Get-CimInstance Win32_Process -Filter \"Name='powershell.exe'\" | "
                      "Where-Object { $_.ParentProcessId -eq %d } | "
                      "ForEach-Object { \"$($_.ProcessId)|$($_.CommandLine)\" }" % self.p.pid)
            out = subprocess.run(["powershell", "-NoProfile", "-Command", script],
                                 capture_output=True, text=True).stdout
            for line in out.splitlines():
                line = line.strip()
                if "|" not in line: continue
                pid, cmd = line.split("|", 1)
                if not pid.isdigit() or "-EncodedCommand" not in cmd: continue
                payload = cmd.split("-EncodedCommand", 1)[1].strip().strip('"')
                try:
                    text = base64.b64decode(payload).decode("utf-16-le")
                except Exception:
                    continue
                texts = re.findall(r"<text id=\"\d\">(.*?)</text>", text)
                if texts:
                    self.toasts.append(tuple(texts))
                    subprocess.run(["taskkill", "/PID", pid, "/F"], capture_output=True)
            time.sleep(0.2)

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

    def drain(self, seconds=1.5):
        end = time.time() + seconds
        while time.time() < end:
            try: line = self.q.get(timeout=0.2)
            except queue.Empty: continue
            if line is None: return
            try: self.frames.append(json.loads(line))
            except ValueError: pass

    def stopped_since(self, since):
        return [f for f in self.frames[since:] if f.get("type") == "stopped"]

    def close(self):
        if self.p.poll() is None:
            self.p.terminate()
            try: self.p.wait(timeout=5)
            except subprocess.TimeoutExpired: self.p.kill()
        self.err.close()

def ended(name):
    def pred(f):
        return (f.get("type") == "line" and f.get("window") == name
                and f.get("kind") == "system"
                and ("idle" in f.get("text", "") or "error" in f.get("text", "")))
    return pred

ui = Client()
try:
    if not ui.wait(lambda f: f.get("type") == "server", 30):
        print("FAIL the client never greeted"); sys.exit(1)

    wait_for(lambda: marks(), 10)
    ok("a file with no marks seeds only the sessions it draws", marks() == {"own": True},
       json.dumps(marks()))

    ui.submit('/new_session --id "fresh" --config \'{"model_config":{"model":"fake/flash-fake"}}\'')
    ok("new_session accepted in this window marks the session its own",
       wait_for(lambda: marks().get("fresh") is True, 15), json.dumps(marks()))

    subprocess.run(["bash", os.path.join(home, "bin", "gongga-cli"), "--home", home,
                    "--server", addr,
                    json.dumps({"name": "set_session",
                                "args": {"id": "server-made", "model": "fake/flash-fake"}})],
                   capture_output=True)
    ui.submit('/set_session --id "server-made" --config \'{"model_config":{"model":"fake/flash-fake"}}\'')
    ui.wait(lambda f: f.get("type") == "line" and "server-made" in f.get("text", ""), 15)
    ui.drain()
    ok("joining an existing session writes no mark", "server-made" not in marks(),
       json.dumps(marks()))

    # An unmarked session's turn end: no notice, and typing to it marks nothing.
    n = len(ui.toasts); since = len(ui.frames)
    ui.submit('/send_message --session "sub/agent" --message "work"')
    end = ui.wait(ended("sub/agent"), 60, since); ui.drain(); time.sleep(0.5)
    ok("an unmarked session's turn end raises no notice",
       end is not None and not any(t[0] == "sub/agent" for t in ui.toasts[n:]),
       "ended=%s toasts=%s" % (bool(end), json.dumps(ui.toasts[n:])))
    ok("an unmarked session's turn end reaches no front-end hook",
       not any(f.get("session") == "sub/agent" for f in ui.stopped_since(since)))
    ok("typing to an unmarked session writes no mark", "sub/agent" not in marks())

    # A marked session's turn end: the CLIENT raises the toast, with no UI attached.
    n = len(ui.toasts); since = len(ui.frames)
    ui.submit('/send_message --session "own" --message "work"')
    stop = ui.wait(lambda f: f.get("type") == "stopped" and f.get("session") == "own", 60, since)
    toast = wait_for(lambda: any(t[0] == "own" for t in ui.toasts[n:]), 10)
    ok("a marked session's turn end raises the client's own toast", toast,
       json.dumps(ui.toasts[n:]))
    ok("the front-end hook still hears the stop", stop is not None, json.dumps(stop))

    # Taking the mark away silences the notice again.
    ui.submit('/set_client_session --session "own" --config \'{"client":false}\'')
    wait_for(lambda: marks().get("own") is False, 10)
    n = len(ui.toasts); since = len(ui.frames)
    ui.submit('/send_message --session "own" --message "work again"')
    end = ui.wait(ended("own"), 60, since); ui.drain(); time.sleep(0.5)
    ok("set_client_session false silences a session that was marked",
       end is not None and not any(t[0] == "own" for t in ui.toasts[n:]),
       json.dumps(marks()))
finally:
    ui.close()

sys.exit(1 if any(not passed for _, passed in checks) else 0)
PY

python "$work/notice-drive.py" "$root" "$work" "$home" "$client_home" "127.0.0.1:$port" \
  "notice-selftest" "$client_bin"
result=$?
say "server pid=$server_pid fake pid=$fake_port"

sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $result -eq 0 ]]; then
  echo "PASS · client-notice · $sha"
else
  echo "FAIL · client-notice · $sha"
fi
exit $result