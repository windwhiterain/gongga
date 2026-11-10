#!/usr/bin/env bash
# Two UI-less clients dial one server at the same time, and closing one of them
# must not cancel a server neither of them started.
#
# Real processes, temporary home, a port from the kernel, an explicit key, and
# one line of verdict at the end. `-v` prints every check.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bin="$root/target/debug"
work="$root/.dev/selftest"
home="$work/multi-home"
log="$work/multi.log"
roster="$work/multi-roster.toml"
key=multi-client-selftest-key
verbose=${1:-}
start=$SECONDS

[[ -x "$bin/gongga-server.exe" || -x "$bin/gongga-server" ]] || {
  echo "build first: cargo build -p gongga-server -p gongga-client" >&2; exit 1; }
[[ -x "$bin/gongga-client.exe" || -x "$bin/gongga-client" ]] || {
  echo "build first: cargo build -p gongga-client" >&2; exit 1; }

server_bin="$bin/gongga-server"; client_bin="$bin/gongga-client"
# The server writes a POSIX shim here, not an .exe: never test it with -x.
cli_bin="$home/bin/gongga-cli"
[[ -x "$server_bin" ]] || server_bin="$bin/gongga-server.exe"
[[ -x "$client_bin" ]] || client_bin="$bin/gongga-client.exe"

alive() { [[ -n "${1:-}" ]] && tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }
child_pid() { tr -d '\r\n' < "$home/server.lock" 2>/dev/null; }
free_port() {
  python -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM);s.bind(("127.0.0.1",0))
print(s.getsockname()[1]);s.close()'
}
wait_listening() {
  for _ in $(seq 1 300); do
    grep -q "listening (UDP) local=127.0.0.1:$port" "$log" && return 0
    sleep 0.2
  done
  return 1
}
# The pid in this home's lock is the only server this script may ever kill.
serving_home() { child_pid; }

port=$(free_port)
# The child's pid comes from this home's lock; the supervisor's pid comes from
# this run's own log, where the server prints `supervisor=<pid>`. Killing only
# the child loses: the watchdog restarts it in 200 ms and the home stays held.
stop_servers() {
  local watch pid
  watch=$(grep -o 'supervisor=[0-9]*' "$log" 2>/dev/null | head -1 | cut -d= -f2)
  pid=$(serving_home)
  [[ -n "${watch:-}" ]] && alive "$watch" \
    && taskkill //PID "$watch" //T //F >/dev/null 2>&1
  [[ -n "${pid:-}" ]] && alive "$pid" \
    && taskkill //PID "$pid" //T //F >/dev/null 2>&1
  sleep 0.5
}
cleanup() {
  stop_servers
  stop_servers
  rm -rf "$home" "$work/ui-a" "$work/ui-b" "$work/ui-remote" \
    "$work/multi-a.err" "$work/multi-b.err" "$work/multi-remote.err" 2>/dev/null || {
    sleep 2
    rm -rf "$home" "$work/ui-a" "$work/ui-b" "$work/ui-remote" \
      "$work/multi-a.err" "$work/multi-b.err" "$work/multi-remote.err" \
      2>/dev/null || echo "note: $home is still held by a process" >&2
  }
}
trap cleanup EXIT

mkdir -p "$work"
# A run that died before its trap gets a clean start: the old log still names
# that run's supervisor, so bring that tree down before reusing the home.
stop_servers
rm -rf "$home" "$work/ui-a" "$work/ui-b" "$work/ui-remote"
mkdir -p "$home"
: > "$log"
export GONGGA_HOME="$home"

cat > "$roster" <<TOML
[[provider]]
id = "fake"
name = "Fake"
key_env = "MULTI_CLIENT_SELFTEST_KEY"
cache_ttl_tiers = { "5m" = 300, "1h" = 3600 }

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
export MULTI_CLIENT_SELFTEST_KEY=sk-fake

# The key is passed to this one process only. Every dial below spells it out,
# because a server started from GONGGA_KEY does not write server.key.
env -u GONGGA_MODEL -u GONGGA_PROVIDER -u GONGGA_BASE_URL -u GONGGA_API_KEY \
  -u GONGGA_API_KEY_ENV GONGGA_KEY="$key" \
  "$server_bin" --home "$home" --bind "127.0.0.1:$port" --log info >>"$log" 2>&1 &
for _ in $(seq 1 150); do [[ -s "$home/server.lock" ]] && break; sleep 0.2; done
wait_listening || { echo "the server never listened on 127.0.0.1:$port" >&2; exit 1; }
server_pid=$(child_pid)

# The driver owns the three client processes and kills only those pids.
# The driver is a Windows-native Python: MSYS `/c/...` paths mean nothing to it.
# `cygpath -m` spells the same files the way both runtimes agree on.
w_client=$(cygpath -m "$client_bin")
w_cli=$(cygpath -m "$cli_bin")
w_home=$(cygpath -m "$home")
w_work=$(cygpath -m "$work")
out=$(python - "$w_client" "$w_cli" "$w_home" "$w_work" \
  "127.0.0.1:$port" "$key" "$server_pid" <<'PY'
import json, os, queue, re, subprocess, sys, threading, time, traceback

client_bin, cli_bin, home, work, addr, key, server_pid = sys.argv[1:8]
lock = os.path.join(home, "server.lock")
checks = []


def ok(name, passed, detail=""):
    checks.append(bool(passed))
    line = "ok   " if passed else "FAIL "
    print("  %s%s%s" % (line, name, (" -- " + detail) if detail and not passed else ""))


def cli(*args):
    # The shim is a #!/bin/sh file; Windows cannot exec it directly.
    return subprocess.run(
        ["bash", cli_bin, "--home", home, "--server", addr, "--key", key, *args],
        capture_output=True, text=True, timeout=20,
    )


def probe(name, tries=25):
    """A third dial answers whether the shared server knows this session."""
    for _ in range(tries):
        r = cli("get_messages", "--session", name)
        if r.returncode == 0:
            return True
        time.sleep(0.2)
    return False


class Peer:
    def __init__(self, tag, *, client_only=True, server=addr):
        self.tag = tag
        self.dir = os.path.join(work, "ui-" + tag)
        os.makedirs(self.dir, exist_ok=True)
        self.err = open(os.path.join(work, "multi-%s.err" % tag), "ab", buffering=0)
        parent = {"a": "990001", "b": "990002", "remote": "990003"}[tag]
        argv = [client_bin, "--home", self.dir, "--client-home", self.dir,
                "--server", server, "--key", key, "--parent", parent]
        if client_only:
            argv.insert(1, "--client_only")
        self.p = subprocess.Popen(argv, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=self.err)
        self.q = queue.Queue()
        self.frames = []
        self.bad = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for raw in self.p.stdout:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line:
                self.q.put(line)
        self.q.put(None)

    def send(self, frame):
        self.p.stdin.write((json.dumps(frame) + "\n").encode())
        self.p.stdin.flush()

    def submit(self, line):
        self.send({"intent": "submit", "line": line})

    def wait(self, pred, timeout=25, since=0):
        end = time.time() + timeout
        while True:
            for f in self.frames[since:]:
                if pred(f):
                    return f
            left = end - time.time()
            if left <= 0:
                return None
            try:
                line = self.q.get(timeout=min(left, 0.25))
            except queue.Empty:
                continue
            if line is None:
                return None
            try:
                self.frames.append(json.loads(line))
            except ValueError:
                self.bad += 1

    def close(self):
        if self.p.poll() is None:
            self.p.terminate()
            try:
                self.p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.p.kill()
                self.p.wait(timeout=5)
        self.err.close()


def is_server(f):
    return f.get("type") == "server"


def frame_pid(f):
    m = re.search(r"#(\d+)", (f.get("label") or ""))
    return int(m.group(1)) if m else None


def read_file(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return ""


def wait_for(path, needle, timeout=8):
    end = time.time() + timeout
    while time.time() < end:
        text = read_file(path)
        if needle in text:
            return text
        time.sleep(0.2)
    return read_file(path)


def run():
    ok("the one-shot cli is installed in this home", os.path.exists(cli_bin))
    if not os.path.exists(cli_bin):
        return
    ok("the server lock names a pid", server_pid.isdigit(), repr(server_pid))
    ok("a session that was never made is refused by the probe",
       not probe("no-such-session-xyz", tries=2))

    a = Peer("a")
    b = Peer("b")
    remote = None
    try:
        fa = a.wait(is_server)
        ok("client A dialled the shared server", fa is not None)
        if fa is None:
            return
        ok("A does not own that server", fa.get("ours") is False, json.dumps(fa))
        ok("A's server label names our server's pid",
           frame_pid(fa) == int(server_pid), json.dumps(fa))

        a.submit("/set_session --id from-a --model fake/flash-fake")
        ok("A's session is on the shared server", probe("from-a"))

        fb = b.wait(is_server)
        ok("client B dialled while A was still connected", fb is not None)
        ok("A was alive when B dialled", a.p.poll() is None)
        if fb is None:
            return
        ok("B does not own that server", fb.get("ours") is False, json.dumps(fb))
        ok("A and B read the very same server", frame_pid(fa) == frame_pid(fb),
           "%s vs %s" % (json.dumps(fa), json.dumps(fb)))

        b.submit("/set_session --id from-b --model fake/flash-fake")
        ok("B's session is on the same shared server", probe("from-b"))
        ok("A's session is still there after B's command", probe("from-a"))

        # Each window keeps its own state. Neither client's sticky entry may
        # appear in the other's client.toml or in the other's frames.
        a.submit("/set_sticky --type session --value a-only")
        b.submit("/set_sticky --type session --value b-only")
        ta = wait_for(os.path.join(a.dir, "client.toml"), "a-only")
        tb = wait_for(os.path.join(b.dir, "client.toml"), "b-only")
        ok("A's own state file holds A's sticky session", "a-only" in ta, ta[:200])
        ok("B's own state file holds B's sticky session", "b-only" in tb, tb[:200])
        ok("B's state did not leak into A's file", "b-only" not in ta)
        ok("A's state did not leak into B's file", "a-only" not in tb)

        # A leaves. It is not the owner, so the server must stay up and B must
        # still be able to write to it.
        a.send({"intent": "quit"})
        try:
            a.p.wait(timeout=10)
            ok("A exited cleanly on quit", a.p.returncode == 0,
               "exit=%s" % a.p.returncode)
        except subprocess.TimeoutExpired:
            ok("A exited cleanly on quit", False, "still running after 10s")
        ok("the server lock still names the same pid",
           read_file(lock).strip() == server_pid, read_file(lock).strip())
        ok("B is still running after A left", b.p.poll() is None)
        ok("the server still answers a third dial", probe("from-a", tries=3))

        mark = len(b.frames)
        b.submit("/set_session --id after-a --model fake/flash-fake")
        ok("B can still write to the shared server", probe("after-a"))
        b.wait(lambda f: "after-a" in json.dumps(f, ensure_ascii=False),
               timeout=5, since=mark)

        ok("neither client started a server of its own",
           not os.path.exists(os.path.join(a.dir, "server.lock"))
           and not os.path.exists(os.path.join(b.dir, "server.lock")))

        # A remote address must neither stop nor spawn a server, even without
        # --client_only. This one never connects; it only has to do no harm.
        remote = Peer("remote", client_only=False, server="192.0.2.1:19999")
        remote.wait(lambda f: f.get("type") == "note", timeout=4)
        ok("a remote address spawns no server",
           not os.path.exists(os.path.join(remote.dir, "server.lock")))
        ok("a remote address did not stop the shared server",
           read_file(lock).strip() == server_pid and remote.p.poll() is None)

        b.send({"intent": "quit"})
        try:
            b.p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
    finally:
        for p in (a, b, remote):
            if p is not None:
                p.close()

    ok("every frame on both pipes was valid JSON", a.bad == 0 and b.bad == 0,
       "bad frames: a=%s b=%s" % (a.bad, b.bad))


try:
    run()
except Exception:
    print("  FAIL python driver -- " + traceback.format_exc().replace("\n", " | "))
    checks.append(False)

sys.exit(0 if checks and all(checks) else 1)
PY
)
py_exit=$?

if [[ "$verbose" == "-v" ]]; then printf '%s\n' "$out"; fi
fails=$(printf '%s\n' "$out" | grep -c '^  FAIL' || true)
[[ $py_exit -ne 0 && $fails -eq 0 ]] && fails=1
sha=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [[ $fails -eq 0 ]]; then
  echo "PASS · $((SECONDS-start))s · multi-client · $sha"
else
  echo "FAIL · $fails check(s) · multi-client · $sha"
fi
exit $(( fails == 0 ? 0 : 1 ))
