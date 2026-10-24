# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""Run commands and move files on remote machines, without a client library.

Why this exists: driving a remote host over ssh is easy to do badly, and the failures
are quiet. Output arrives in an encoding nobody declared, an exit code becomes 1, a
quote gets eaten by one of three shells, or the call hangs forever because ssh
inherited a stdin that never closes. Each of those costs a debugging session, and none
of them is about the remote machine at all.

So the shape here is deliberate:

  * A script is sent as base64 and written to a file on the remote side before it runs.
    Nothing about it passes through a shell's parser on the way, so there is no quoting
    layer to get wrong -- the same reason `-EncodedCommand` exists on Windows, applied
    once for every shell family.
  * Output comes back base64-framed, so it is bytes rather than "text in whatever code
    page the remote console had". A GBK Windows console no longer produces mojibake.
  * The exit code is carried explicitly in the reply, because a channel that only
    reports "ssh failed" loses the difference between 1 and 7.
  * stdin is never inherited: either the transport owns it (a session) or it is
    /dev/null (a one-shot). The caller's stdin is a pipe that may never close.

Two paths, one transport:

  * one-shot -- one ssh per call, ~0.9 s here, and it always works;
  * session  -- one ssh held open per target, ~20-40 ms per call, used when it can be.

The session is an optimization on top of the one-shot, never a dependency: if it is
unavailable, dead, or was never started, the same command goes through the one-shot
path and says so.

Nothing here reads ~/.ssh/config, a roster, or any other local convention. A target is
whatever you tell it: host, port, user, identity. Store it once and it is just a name.
"""

import argparse
import base64
import json
import os
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid

STATE_DIR = os.path.join(os.path.expanduser("~"), ".remote")
TARGETS_PATH = os.path.join(STATE_DIR, "targets.json")
SESSIONS_DIR = os.path.join(STATE_DIR, "sessions")
LOGS_DIR = os.path.join(STATE_DIR, "logs")

POSIX = "posix"
POWERSHELL = "powershell"
DEFAULT_TIMEOUT = 60.0
DEFAULT_MAX_OUTPUT = 4 * 1024 * 1024
CHUNK = 3000
IDLE_TIMEOUT = 30 * 60


class RemoteError(Exception):
    """Something the caller can act on: a bad target, a dead link, a lost outcome."""


# --- targets -------------------------------------------------------------------


def load_targets():
    try:
        with open(TARGETS_PATH, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def save_targets(targets):
    os.makedirs(STATE_DIR, exist_ok=True)
    temporary = TARGETS_PATH + ".tmp"
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(targets, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(temporary, TARGETS_PATH)


def normalize_target(raw):
    """Fill in the parts of a target that have an obvious default, and nothing else.

    A default that is wrong is worse than a required field: `--port` omitted means 22
    for every ssh server ever shipped, while `--user` omitted means "whatever the
    client machine's account is called", which is usually not the remote account.
    """
    target = dict(raw)
    target.setdefault("port", 22)
    target.setdefault("user", None)
    target.setdefault("identity", None)
    target.setdefault("shell", "auto")
    target.setdefault("runner", None)
    target.setdefault("login", "auto")
    target.setdefault("strict", "accept-new")
    target.setdefault("jump", None)
    target.setdefault("no_config", False)
    target.setdefault("connect_timeout", 10)
    if not target.get("host"):
        raise RemoteError("a target needs a host")
    if target["shell"] not in ("auto", POSIX, POWERSHELL):
        raise RemoteError("shell must be auto, posix or powershell")
    if target["login"] not in ("auto", POSIX, POWERSHELL, "cmd"):
        raise RemoteError("login must be auto, posix, powershell or cmd")
    if target["runner"] and target["shell"] == POWERSHELL:
        raise RemoteError("--runner names a POSIX interpreter; it does not apply to powershell")
    return target


LOGIN_TRAILERS = {
    # A login shell decides what ssh returns, and two of the three families answer 1
    # for any failed command unless the line itself says otherwise. Measured on a host
    # whose sshd runs PowerShell: `exit 7` inside a payload came back as 1, and adding
    # `; exit $LASTEXITCODE` brought back 7. A POSIX login needs no trailer at all --
    # `sh -c` already reports the last command's status.
    POSIX: "",
    POWERSHELL: "; exit $LASTEXITCODE",
    "cmd": "; exit /b %ERRORLEVEL%",
}


def ssh_command(target, inner):
    """The string sshd hands to the login shell: the payload plus its trailer."""
    return inner + LOGIN_TRAILERS.get(target.get("login") or POSIX, "")


def destination(target):
    user = target.get("user")
    host = target["host"]
    return "%s@%s" % (user, host) if user else host


def ssh_base(target, interactive_stdin=False):
    """The ssh command line, built only from the target itself.

    BatchMode is on by default so a missing key fails in a second instead of waiting at
    a prompt that nobody can see. Everything that would make ssh consult local
    conventions is a flag the caller can set, not a file we read.
    """
    argv = [
        "ssh",
        "-T",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=%d" % target["connect_timeout"],
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=3",
        "-o", "StrictHostKeyChecking=%s" % target["strict"],
        "-o", "LogLevel=ERROR",
    ]
    if not interactive_stdin:
        argv.append("-n")
    if target.get("port") and target["port"] != 22:
        argv += ["-p", str(target["port"])]
    if target.get("identity"):
        argv += ["-i", os.path.expanduser(target["identity"])]
    if target.get("jump"):
        argv += ["-J", target["jump"]]
    if target.get("no_config"):
        argv += ["-F", "/dev/null"]
    argv.append(destination(target))
    return argv


def sftp_base(target):
    argv = [
        "sftp",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=%d" % target["connect_timeout"],
        "-o", "StrictHostKeyChecking=%s" % target["strict"],
        "-o", "LogLevel=ERROR",
        "-b", "-",
    ]
    if target.get("port") and target["port"] != 22:
        argv += ["-P", str(target["port"])]
    if target.get("identity"):
        argv += ["-i", os.path.expanduser(target["identity"])]
    if target.get("jump"):
        argv += ["-J", target["jump"]]
    if target.get("no_config"):
        argv += ["-F", "/dev/null"]
    argv.append(destination(target))
    return argv


# --- wrapping a script ---------------------------------------------------------


def posix_quote(value):
    """Single-quote for any Bourne shell: the only character with meaning is `'`."""
    return "'" + str(value).replace("'", "'\\''") + "'"


def powershell_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def wrap_posix(script, cwd=None, env=None):
    lines = []
    if env:
        for key, value in env.items():
            lines.append("export %s=%s" % (key, posix_quote(value)))
    if cwd:
        lines.append("cd %s || { echo \"remote: no such directory: %s\" >&2; exit 96; }" % (posix_quote(cwd), cwd))
    lines.append(script)
    return "\n".join(lines)


def wrap_powershell(script, cwd=None, env=None):
    """Compose a PowerShell payload whose exit code is the script's, not PowerShell's.

    The trailer matters: `-File script.ps1` reports 1 for any failure, so a script that
    means to fail with 7 has to say so itself. Without this the caller cannot tell a
    validation failure from a crash.
    """
    lines = ["$ErrorActionPreference = 'Stop'"]
    try:
        lines.append("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)")
        lines.append("$OutputEncoding = [Text.UTF8Encoding]::new($false)")
    except Exception:
        pass
    if env:
        for key, value in env.items():
            lines.append("$env:%s = %s" % (key, powershell_quote(value)))
    if cwd:
        lines.append("Set-Location -LiteralPath %s" % powershell_quote(cwd))
    lines.append(script)
    lines.append("if ($null -ne $LASTEXITCODE) { exit $LASTEXITCODE } else { exit 0 }")
    return "\n".join(lines)


def wrap(script, shell, cwd=None, env=None):
    return wrap_posix(script, cwd, env) if shell == POSIX else wrap_powershell(script, cwd, env)


def b64(text):
    return base64.b64encode(text.encode("utf-8") if isinstance(text, str) else text).decode("ascii")


# --- the remote loops ----------------------------------------------------------


def posix_bootstrap(script_path_hint="/tmp"):
    """A POSIX loop that answers a line protocol, with no assumption past /bin/sh."""
    return r'''
D=${TMPDIR:-/tmp}/.remote-loop-$$
mkdir -p "$D" 2>/dev/null || exit 90
trap 'rm -rf "$D" 2>/dev/null' EXIT

enc() { # $1=path  -> base64 on stdout
  if command -v base64 >/dev/null 2>&1; then base64 < "$1"
  elif command -v openssl >/dev/null 2>&1; then openssl base64 -A < "$1"
  else return 1; fi
}

emit() { # $1=tag $2=id $3=path
  [ -s "$3" ] || return 0
  enc "$3" 2>/dev/null | fold -w CHUNKWIDTH 2>/dev/null | while IFS= read -r c; do
    [ -n "$c" ] && printf '%s %s %s\n' "$1" "$2" "$c"
  done
}

while IFS= read -r line; do
  case $line in
    R\ *)
      rest=${line#R }
      id=${rest%% *}
      payload=${rest#* }
      printf '%s' "$payload" | tr -d '\r\n' > "$D/.b64" 2>/dev/null
      if command -v base64 >/dev/null 2>&1; then base64 -d < "$D/.b64" > "$D/script" 2>/dev/null
      else openssl base64 -d -A < "$D/.b64" > "$D/script" 2>/dev/null; fi
      sh "$D/script" > "$D/out" 2> "$D/err" < /dev/null
      rc=$?
      emit O "$id" "$D/out"
      emit E "$id" "$D/err"
      printf 'X %s %s\n' "$id" "$rc"
      ;;
    Q) exit 0 ;;
  esac
done
'''.replace("CHUNKWIDTH", str(CHUNK))


def powershell_bootstrap():
    """The same protocol, driven by a PowerShell loop.

    Two decisions here are load-bearing, both measured:

    * The child is started through `System.Diagnostics.Process`, not `Start-Process
      -Wait`. `Start-Process` cost ~2.1 s per request on PowerShell 5.1, which is the
      entire budget this session exists to save.
    * The child's stdin is redirected to an empty pipe and closed. In a real console a
      child inherits the console's handles; over ssh that means it inherits *this
      channel*, and a child that reads stdin eats the next request. The protocol then
      stops answering while looking perfectly healthy.
    * Output is copied as **bytes** (`StandardOutput.BaseStream`), not through the
      reader, which decodes with the console code page and would corrupt binary output.
    """
    script = r'''
$ErrorActionPreference = 'Continue'
$dir = Join-Path ([IO.Path]::GetTempPath()) ("remote-loop-" + $PID)
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$utf8 = New-Object System.Text.UTF8Encoding $false
$script:ps = (Get-Process -Id $PID).Path

function Emit([string]$tag, [string]$id, [byte[]]$bytes) {
  if ($null -eq $bytes -or $bytes.Length -eq 0) { return }
  $b = [Convert]::ToBase64String($bytes)
  for ($i = 0; $i -lt $b.Length; $i += CHUNKWIDTH) {
    $len = [Math]::Min(CHUNKWIDTH, $b.Length - $i)
    [Console]::Out.WriteLine($tag + ' ' + $id + ' ' + $b.Substring($i, $len))
  }
}
function Read-Bytes([string]$path) {
  if (-not (Test-Path -LiteralPath $path)) { return [byte[]]@() }
  try { return [IO.File]::ReadAllBytes($path) } catch { return [byte[]]@() }
}

$reader = [Console]::In
while ($null -ne ($line = $reader.ReadLine())) {
  if ($line -eq 'Q') { break }
  if ($line -notmatch '^R (\S+) (\S+)$') { continue }
  $id = $Matches[1]
  $payload = $Matches[2]
  $sp = Join-Path $dir 'script.ps1'
  $outPath = Join-Path $dir 'out'
  $errPath = Join-Path $dir 'err'
  try {
    $text = $utf8.GetString([Convert]::FromBase64String($payload))
    [IO.File]::WriteAllText($sp, $text, $utf8)
  } catch {
    Emit 'E' $id ($utf8.GetBytes("remote: could not decode the script: " + $_.Exception.Message + [Environment]::NewLine))
    [Console]::Out.WriteLine('X ' + $id + ' 95')
    continue
  }
  $rc = 95
  $outStream = $null; $errStream = $null
  try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $script:ps
    $psi.Arguments = '-NoLogo -NoProfile -ExecutionPolicy Bypass -File "' + $sp + '"'
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardInput = $true
    $psi.StandardOutputEncoding = $utf8
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $psi
    [void]$process.Start()
    # Closing the child's stdin at once is what keeps it from eating this channel.
    $process.StandardInput.Close()
    $outStream = [IO.File]::Create($outPath)
    $errStream = [IO.File]::Create($errPath)
    $copyOut = $process.StandardOutput.BaseStream.CopyToAsync($outStream)
    $copyErr = $process.StandardError.BaseStream.CopyToAsync($errStream)
    $process.WaitForExit()
    $copyOut.Wait()
    $copyErr.Wait()
    $rc = $process.ExitCode
  } catch {
    [IO.File]::WriteAllText($errPath, "remote: could not run the script: " + $_.Exception.Message, $utf8)
  } finally {
    if ($outStream) { $outStream.Close() }
    if ($errStream) { $errStream.Close() }
  }
  Emit 'O' $id (Read-Bytes $outPath)
  Emit 'E' $id (Read-Bytes $errPath)
  [Console]::Out.WriteLine('X ' + $id + ' ' + $rc)
}
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
'''.replace("CHUNKWIDTH", str(CHUNK))
    return script


def shell_invocation(target, inner):
    """Wrap an inner command line so that both the login shell and a named interpreter agree.

    Without a runner, the login shell is the interpreter and each family gets its own
    encoding. With a runner -- a Windows host whose Git Bash is wanted, say -- the inner
    line is handed to that program, so it has to survive the *login* shell's quoting
    too. It is written with no `$`, no backtick and no `?` for exactly that reason, and
    quoted with the login shell's own convention.
    """
    runner = target.get("runner")
    if not runner:
        return None
    # cmd.exe has no single-quote quoting at all and passes quotes through verbatim, so
    # a POSIX runner on a Windows login shell needs the double-quoted form.
    quote = '"' if (target.get("login") or POSIX) in (POWERSHELL, "cmd") else "'"
    return "%s -c %s%s%s" % (runner, quote, inner, quote)


def bootstrap_command(shell, target=None):
    """The one line that starts a loop, in the shape each login shell can actually run.

    Measured, and the difference is not cosmetic -- but it is **Powershell-specific**. A
    Powershell started as a child with an empty stdin concludes EOF at startup and exits
    before the loop can read anything; a request written two seconds later then reaches
    nothing, and the channel looks open while answering nothing. The same loop evaluated
    inside the login shell reads late input correctly, so for a Powershell login the loop
    code runs there instead.

    `sh` does not behave this way -- a child `sh` was measured reading a 2 s-delayed
    line normally -- so the POSIX branch keeps using one. Generalising the Powershell
    fix to `sh` would have been a guess that happened to be harmless, and the comment
    would have taught the wrong reason.

    Each family gets the encoding it already trusts: a POSIX shell decodes base64 and
    pipes it into `sh`; Powershell gets its code base64'd through Invoke-Expression, or
    `-EncodedCommand` when it has to be a child.
    """
    login = (target or {}).get("login") or POSIX

    if shell == POSIX:
        payload = b64(posix_bootstrap())
        if target is not None and target.get("runner"):
            # `exec` hands the channel to the interpreter, so ssh's exit status and the
            # loop's stdin are both exactly what they would have been without it.
            return shell_invocation(
                target,
                "printf %%s %s | base64 -d > /tmp/.remote-boot && exec sh /tmp/.remote-boot" % payload,
            )
        # A single line with no quotes: the base64 alphabet is safe in sh, bash, zsh,
        # csh and fish alike, so the login shell cannot reinterpret it.
        #
        # A child `sh` is fine here, which was measured rather than assumed: with a
        # write delayed 2 s, `sh script` reads it normally. The start-up-EOF problem
        # that forces the Powershell branch below to avoid a child does not apply to
        # `sh` -- see the note there.
        return (
            "D=${TMPDIR:-/tmp}/.remote-boot-$$; "
            "{ printf %%s %s | base64 -d > $D; } 2>/dev/null || "
            "{ printf %%s %s | openssl base64 -d -A > $D; }; "
            "exec sh $D" % (payload, payload)
        )

    payload = b64(powershell_bootstrap())
    if login == POWERSHELL:
        # Runs in the login shell itself: no child, so no startup EOF.
        return (
            "$__remote = [Text.Encoding]::UTF8.GetString("
            "[Convert]::FromBase64String('%s')); Invoke-Expression $__remote" % payload
        )
    encoded = base64.b64encode(powershell_bootstrap().encode("utf-16-le")).decode("ascii")
    return "powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -EncodedCommand " + encoded


def oneshot_command(script_text, shell, target=None):
    """One command line that runs a script and reports its exit code, with no quoting.

    The script is decoded to a file first, so nothing in it is ever parsed by the login
    shell -- which on a Windows host may be PowerShell, cmd, or something a person set
    as their default years ago.
    """
    if shell == POSIX:
        payload = b64(script_text)
        if target is not None and target.get("runner"):
            return shell_invocation(
                target,
                "printf %%s %s | base64 -d > /tmp/.remote-one && sh /tmp/.remote-one" % payload,
            )
        return (
            "D=/tmp/.remote-one-$$; "
            "{ printf %%s %s | base64 -d > $D; } 2>/dev/null || "
            "{ printf %%s %s | openssl base64 -d -A > $D; }; "
            "sh $D; rc=$?; rm -f $D; exit $rc" % (payload, payload)
        )
    encoded = base64.b64encode(script_text.encode("utf-16-le")).decode("ascii")
    return "powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand " + encoded


# --- the session: one ssh held open --------------------------------------------


class Session(object):
    """One ssh channel, reused, so a command costs a round trip instead of a login.

    Replies are framed by a per-request id rather than by "the next output". Without
    that, a request that times out leaves unread frames in the pipe and every later
    reply is off by one -- which presents as "the remote returned another command's
    output", a bug that is nearly impossible to see from the outside.
    """

    def __init__(self, target, shell):
        self.target = target
        self.shell = shell
        self.process = None
        self.frames = []
        self.lock = threading.Lock()
        self.dead = False
        self.noise = []
        self.opened = 0

    def open(self):
        argv = ssh_base(self.target, interactive_stdin=True) + [
            ssh_command(self.target, bootstrap_command(self.shell, self.target))
        ]
        self.process = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )
        self.frames = []
        self.noise = []
        self.dead = False
        self.opened += 1
        threading.Thread(target=self._drain_stdout, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()
        return self

    def _drain_stdout(self):
        """Read the channel in blocks and split lines here.

        `readline()` on an unbuffered pipe is implemented a byte at a time, and with
        `bufsize=0` it also interacts badly with a Windows pipe: the same channel that
        answers the first request stops answering the second while ssh is perfectly
        healthy. Reading fixed blocks and framing them here removes that entire class
        of problem.
        """
        fd = self.process.stdout.fileno()
        pending = b""
        while True:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                self.dead = True
                return
            if not chunk:
                self.dead = True
                return
            pending += chunk
            while b"\n" in pending:
                raw, _, pending = pending.partition(b"\n")
                text = raw.decode("ascii", "replace").strip()
                if text:
                    with self.lock:
                        self.frames.append(text)

    def _drain_stderr(self):
        fd = self.process.stderr.fileno()
        pending = b""
        while True:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                return
            if not chunk:
                return
            pending += chunk
            while b"\n" in pending:
                raw, _, pending = pending.partition(b"\n")
                text = raw.decode("utf-8", "replace").rstrip()
                if text:
                    with self.lock:
                        self.noise.append(text)

    def alive(self):
        return not self.dead and self.process is not None and self.process.poll() is None

    def close(self):
        """Ask the loop to stop, then stop waiting for it to agree."""
        if not self.process:
            return
        try:
            self.process.stdin.write(b"Q\n")
            self.process.stdin.flush()
        except (OSError, ValueError):
            pass
        for step in (self.process.stdin, self.process.stdout, self.process.stderr):
            try:
                if step:
                    step.close()
            except (OSError, ValueError):
                pass
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
        self.dead = True

    def kill(self):
        if self.process and self.process.poll() is None:
            try:
                self.process.kill()
            except OSError:
                pass
        self.dead = True

    def take_frames(self):
        with self.lock:
            frames, self.frames = self.frames, []
            noise, self.noise = self.noise, []
        return frames, noise

    def run(self, script, timeout, max_output):
        if not self.alive():
            self.open()
        request = uuid.uuid4().hex[:12]
        payload = "R %s %s\n" % (request, b64(script))
        try:
            self.process.stdin.write(payload.encode("ascii"))
            self.process.stdin.flush()
        except (OSError, ValueError) as error:
            self.dead = True
            raise RemoteError("the session could not send the request: %s" % error)

        started = time.time()
        out, err, exit_code, truncated = [], [], None, False
        size = 0
        deadline = started + timeout
        while True:
            if time.time() > deadline:
                # The remote may well have run the command. Killing the channel is the
                # only way to get the transport back, and the caller is told plainly
                # that the outcome is unknown rather than retried -- a retry could run
                # the command twice.
                self.kill()
                return {
                    "stdout": b"".join(out),
                    "stderr": b"".join(err),
                    "exit": None,
                    "timedOut": True,
                    "truncated": truncated,
                    "ms": int((time.time() - started) * 1000),
                    "reopened": False,
                    "outcomeUnknown": True,
                }
            frames, noise = self.take_frames()
            for line in frames:
                parts = line.split(" ", 2)
                if len(parts) < 2 or parts[1] != request:
                    if parts[0] not in ("O", "E", "X"):
                        continue
                    continue
                tag = parts[0]
                if tag == "X":
                    try:
                        exit_code = int(parts[2] if len(parts) > 2 else 0)
                    except ValueError:
                        exit_code = None
                    break
                body = parts[2] if len(parts) > 2 else ""
                try:
                    chunk = base64.b64decode(body)
                except Exception:
                    continue
                if tag == "O":
                    if size < max_output:
                        room = max_output - size
                        if len(chunk) > room:
                            out.append(chunk[:room])
                            size = max_output
                            truncated = True
                        else:
                            out.append(chunk)
                            size += len(chunk)
                else:
                    err.append(chunk)
            if exit_code is not None:
                break
            if self.dead:
                raise RemoteError(
                    "the session died while the request was in flight; its outcome on the "
                    "remote is unknown. It will not be replayed. Noise: %s" % ("; ".join(noise[-3:]) or "none")
                )
            time.sleep(0.002)
        return {
            "stdout": b"".join(out),
            "stderr": b"".join(err),
            "exit": exit_code,
            "timedOut": False,
            "truncated": truncated,
            "ms": int((time.time() - started) * 1000),
            "reopened": False,
            "outcomeUnknown": False,
        }


def session_state_path(name):
    safe = "".join(character if character.isalnum() or character in "-._" else "_" for character in name)
    return os.path.join(SESSIONS_DIR, safe + ".json")


def read_session_state(name):
    try:
        with open(session_state_path(name), "r", encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return None


def write_session_state(name, state):
    os.makedirs(SESSIONS_DIR, exist_ok=True)
    path = session_state_path(name)
    temporary = path + ".tmp"
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2)
        handle.write("\n")
    os.replace(temporary, path)


def clear_session_state(name):
    try:
        os.unlink(session_state_path(name))
    except OSError:
        pass


def call_daemon(port, request, timeout):
    """Send one request to a running daemon and read one reply."""
    with socket.create_connection(("127.0.0.1", port), timeout=5) as connection:
        connection.settimeout(timeout + 30)
        connection.sendall((json.dumps(request) + "\n").encode("utf-8"))
        buffer = b""
        while not buffer.endswith(b"\n"):
            chunk = connection.recv(65536)
            if not chunk:
                break
            buffer += chunk
    if not buffer.strip():
        raise RemoteError("the session closed without answering")
    return json.loads(buffer.decode("utf-8"))


def daemon_alive(name):
    state = read_session_state(name)
    if not state:
        return None
    try:
        port = int(state["port"])
    except (KeyError, ValueError, TypeError):
        return None
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=2):
            return state
    except OSError:
        clear_session_state(name)
        return None


def start_daemon(name, wait=6.0):
    """Spawn the detached process that owns the ssh channel.

    Detached on purpose: it outlives this invocation, which is the entire point -- a
    session whose lifetime is one tool call is just a slow one-shot.
    """
    os.makedirs(LOGS_DIR, exist_ok=True)
    log_path = os.path.join(LOGS_DIR, name + ".log")
    log = open(log_path, "ab", buffering=0)
    argv = [sys.executable, os.path.abspath(__file__), "_daemon", name]
    kwargs = {
        "stdin": subprocess.DEVNULL,
        "stdout": log,
        "stderr": log,
        "close_fds": True,
    }
    if os.name == "nt":
        kwargs["creationflags"] = 0x00000008 | 0x00000200  # DETACHED_PROCESS | NEW_PROCESS_GROUP
    else:
        kwargs["start_new_session"] = True
    subprocess.Popen(argv, **kwargs)
    log.close()
    deadline = time.time() + wait
    while time.time() < deadline:
        state = daemon_alive(name)
        if state:
            return state
        time.sleep(0.05)
    raise RemoteError(
        "the session for %r did not come up; see %s" % (name, log_path)
    )


def session_possible(target):
    """Whether a session can be offered for this target at all.

    A runner names an interpreter for one-shot calls; a session speaks the login shell's
    own family instead. Offering both would mean a session that silently runs something
    other than what the target asked for.
    """
    if target.get("runner"):
        return False, "this target runs scripts through %s, so it uses one-shot calls" % target["runner"]
    return True, ""


def ensure_daemon(name):
    state = daemon_alive(name)
    return state or start_daemon(name)


# --- the daemon process --------------------------------------------------------


def serve(name):
    targets = load_targets()
    if name not in targets:
        print("remote: no such target: %s" % name, file=sys.stderr)
        return 2
    target = normalize_target(targets[name])
    shell = target["shell"]
    if shell == "auto":
        shell = detect_shell(target)
        target["shell"] = shell

    session = Session(target, shell)
    session.open()

    # Announce nothing until the loop has answered once. A channel that starts but
    # never replies is indistinguishable from a slow one from the outside, and that is
    # exactly the failure this check turns into an honest error.
    try:
        probe = session.run(wrap("echo __remote_ready__", shell), 20, 65536)
    except RemoteError as error:
        print("remote: the session opened but did not answer: %s" % error, file=sys.stderr)
        session.close()
        return 3
    if b"__remote_ready__" not in (probe.get("stdout") or b""):
        print("remote: the session opened but the loop did not answer the probe", file=sys.stderr)
        session.close()
        return 3

    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    server.listen(8)
    server.settimeout(5.0)
    write_session_state(name, {
        "pid": os.getpid(),
        "port": server.getsockname()[1],
        "target": name,
        "shell": shell,
        "startedAt": time.strftime("%Y-%m-%dT%H:%M:%S"),
    })

    idle_deadline = time.time() + IDLE_TIMEOUT
    try:
        while True:
            if time.time() > idle_deadline:
                break
            try:
                connection, _ = server.accept()
            except socket.timeout:
                continue
            with connection:
                try:
                    request = read_json_line(connection)
                except (OSError, ValueError):
                    continue
                if request.get("cmd") == "shutdown":
                    write_json_line(connection, {"ok": True})
                    break
                if request.get("cmd") == "ping":
                    write_json_line(connection, {
                        "ok": True, "pid": os.getpid(), "shell": shell,
                        "alive": session.alive(), "opened": session.opened,
                    })
                    continue
                idle_deadline = time.time() + IDLE_TIMEOUT
                script = request.get("script") or ""
                timeout = float(request.get("timeout") or DEFAULT_TIMEOUT)
                max_output = int(request.get("max_output") or DEFAULT_MAX_OUTPUT)
                try:
                    before = session.opened
                    result = session.run(script, timeout, max_output)
                    result["reopened"] = session.opened != before
                    result["ok"] = True
                except RemoteError as error:
                    result = {"ok": False, "error": str(error)}
                write_json_line(connection, encode_result(result))
    finally:
        session.close()
        clear_session_state(name)
        server.close()
    return 0


def encode_result(result):
    payload = dict(result)
    for key in ("stdout", "stderr"):
        if isinstance(payload.get(key), bytes):
            payload[key] = base64.b64encode(payload[key]).decode("ascii")
    return payload


def decode_result(payload):
    result = dict(payload)
    for key in ("stdout", "stderr"):
        if isinstance(payload.get(key), str):
            try:
                result[key] = base64.b64decode(payload[key])
            except Exception:
                result[key] = b""
    return result


def read_json_line(connection):
    buffer = b""
    while not buffer.endswith(b"\n"):
        chunk = connection.recv(65536)
        if not chunk:
            break
        buffer += chunk
        if len(buffer) > 64 * 1024 * 1024:
            raise ValueError("request too large")
    return json.loads(buffer.decode("utf-8"))


def write_json_line(connection, payload):
    connection.sendall((json.dumps(payload) + "\n").encode("utf-8"))


# --- running it ----------------------------------------------------------------


# A Windows command line is capped at 32,767 characters, and the payload is base64
# (~4/3 of the script). Exceeding it fails as a truncation, not an error, so the limit
# is checked here and reported in terms of what the caller can do about it.
COMMAND_LINE_BUDGET = 24000


def run_oneshot(target, shell, script_text, timeout, max_output):
    """One ssh, one command, exact exit code."""
    command = ssh_command(target, oneshot_command(script_text, shell, target))
    if len(command) > COMMAND_LINE_BUDGET:
        raise RemoteError(
            "this script is %d characters once encoded, past the %.0f a command line can carry; "
            "copy it with `put` and run it as a file, or use a session"
            % (len(command), COMMAND_LINE_BUDGET)
        )
    argv = ssh_base(target) + [command]
    started = time.time()
    try:
        completed = subprocess.run(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout + 15,
        )
    except subprocess.TimeoutExpired:
        return {
            "stdout": b"", "stderr": b"", "exit": None, "timedOut": True,
            "truncated": False, "ms": int((time.time() - started) * 1000),
            "outcomeUnknown": True, "path": "oneshot",
        }
    stdout, stderr = completed.stdout, completed.stderr
    truncated = len(stdout) > max_output
    if truncated:
        stdout = stdout[:max_output]
    return {
        "stdout": stdout,
        "stderr": stderr,
        "exit": completed.returncode,
        "timedOut": False,
        "truncated": truncated,
        "ms": int((time.time() - started) * 1000),
        "outcomeUnknown": False,
        "path": "oneshot",
    }


def detect_shell(target, timeout=25.0):
    """Which family the remote understands, decided by trying rather than by inferring.

    There is no portable way to ask "what is your login shell" across ssh, and the
    answer decides how every later script is encoded, so it is measured once. The probe
    is a shell-neutral `echo`: what distinguishes the families is whether the POSIX
    framing is understood at all.
    """
    probe = "echo __remote_probe__"
    for shell in (POSIX, POWERSHELL):
        result = run_oneshot(target, shell, wrap(probe, shell), timeout, 64 * 1024)
        if result["exit"] == 0 and b"__remote_probe__" in result["stdout"]:
            return shell
    raise RemoteError(
        "the remote answered neither a POSIX nor a PowerShell probe; "
        "pass --shell explicitly if it is one of those"
    )


def detect_login(target, shell, timeout=25.0):
    """Which shell sshd puts in front, decided by asking it to report a failure.

    This is a separate question from which interpreter runs the payload, and it has to
    be answered by experiment: a PowerShell login turns a child that exited 7 into 1
    unless the line ends with `exit $LASTEXITCODE`, while a POSIX login needs no trailer
    and `cmd` needs a different one again.
    """
    for candidate in (POSIX, POWERSHELL, "cmd"):
        probe = dict(target)
        probe["login"] = candidate
        line = ssh_command(probe, oneshot_command("exit 7", shell, probe))
        try:
            completed = subprocess.run(
                ssh_base(probe) + [line],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            continue
        if completed.returncode == 7:
            return candidate
    raise RemoteError(
        "could not make the login shell report an exit code; pass --login explicitly"
    )


def resolve(name, target, shell_override=None, login_override=None):
    """Fill in what has to be measured, remember it, and return the usable target.

    Detection costs a connection, so it happens once and lands in the target file. That
    also means a later failure is a *change* on the remote, not a repeated guess.
    """
    resolved = dict(target)
    if shell_override:
        resolved["shell"] = shell_override
    if login_override:
        resolved["login"] = login_override
    changed = {}
    if resolved["shell"] == "auto":
        resolved["shell"] = detect_shell(resolved)
        changed["shell"] = resolved["shell"]
    if resolved["login"] == "auto":
        resolved["login"] = detect_login(resolved, resolved["shell"])
        changed["login"] = resolved["login"]
    if changed:
        targets = load_targets()
        if name in targets:
            targets[name].update(changed)
            save_targets(targets)
    return resolved


def execute(name, target, shell, script, timeout, max_output, prefer_session=True):
    """Run a script the cheap way that is available, and say which way that was."""
    wrapped = script if isinstance(script, str) else script
    if prefer_session and not session_possible(target)[0]:
        prefer_session = False
    if prefer_session:
        state = ensure_daemon(name)
        reply = call_daemon(state["port"], {
            "cmd": "run",
            "script": wrapped,
            "timeout": timeout,
            "max_output": max_output,
        }, timeout)
        result = decode_result(reply)
        result["path"] = "session"
        return result
    return run_oneshot(target, shell, wrapped, timeout, max_output)


# --- files ---------------------------------------------------------------------


def sftp_quote(path):
    """Quote one path for an sftp batch line.

    sftp has its own parser, not a shell's: double quotes group, and a backslash
    escapes inside them. A Windows path's backslashes therefore have to be doubled,
    which is exactly the kind of detail that turns into "the file went to the wrong
    place" instead of an error.
    """
    text = str(path)
    if text and all(character not in ' \t"\'\\' for character in text):
        return text
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def sftp_batch(target, lines, timeout):
    argv = sftp_base(target)
    body = "\n".join(lines) + "\n"
    try:
        completed = subprocess.run(
            argv,
            input=body.encode("utf-8"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        raise RemoteError("the transfer did not finish in %.0fs" % timeout)
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip() or completed.stdout.decode("utf-8", "replace").strip()
        raise RemoteError("sftp failed: %s" % (detail or "exit %d" % completed.returncode))
    return completed


def remote_size(target, shell, path, timeout):
    """The remote file's size, asked in the remote shell's own words."""
    if shell == POSIX:
        script = "wc -c < %s | tr -d ' '" % posix_quote(path)
    else:
        script = "Write-Output (Get-Item -LiteralPath %s).Length" % powershell_quote(path)
    result = run_oneshot(target, shell, wrap(script, shell), timeout, 64 * 1024)
    text = result["stdout"].decode("utf-8", "replace").strip()
    try:
        return int(text.splitlines()[-1])
    except (IndexError, ValueError):
        return None


def put_file(target, shell, local, remote, timeout=300.0, verify=True, mode=None):
    size = os.path.getsize(local)
    lines = ["put %s %s" % (sftp_quote(local), sftp_quote(remote))]
    if mode:
        lines.append("chmod %s %s" % (mode, sftp_quote(remote)))
    sftp_batch(target, lines, timeout)
    if verify:
        # A transfer that reports success is not the same as a file that arrived: the
        # measured failure mode on this pair was a truncated copy that exited 0.
        actual = remote_size(target, shell, remote, timeout)
        if actual is None:
            return {"verified": False, "bytes": size, "note": "could not read the size back"}
        if actual != size:
            raise RemoteError(
                "the copy is %d bytes but the original is %d -- it did not arrive intact"
                % (actual, size)
            )
    return {"verified": verify, "bytes": size}


def get_file(target, shell, remote, local, timeout=300.0, verify=True):
    expected = remote_size(target, shell, remote, timeout) if verify else None
    sftp_batch(target, ["get %s %s" % (sftp_quote(remote), sftp_quote(local))], timeout)
    actual = os.path.getsize(local)
    if verify and expected is not None and actual != expected:
        raise RemoteError("the copy is %d bytes but the remote file is %d" % (actual, expected))
    return {"verified": verify, "bytes": actual}


# --- background jobs -----------------------------------------------------------


def detach(target, shell, script, timeout=60.0):
    """Start something that outlives the call, and return how to watch it.

    The command is base64'd into a launcher, so the launcher itself has no quoting in
    it: what runs in the background is exactly the text that was passed in.
    """
    stamp = time.strftime("%Y%m%d-%H%M%S") + "-" + uuid.uuid4().hex[:4]
    if shell == POSIX:
        launcher = (
            'B=/tmp/remote-job-%s.sh; L=/tmp/remote-job-%s.log; '
            '{ printf %%s %s | base64 -d > "$B"; } 2>/dev/null || '
            '{ printf %%s %s | openssl base64 -d -A > "$B"; }; '
            'nohup sh "$B" > "$L" 2>&1 < /dev/null & echo "$! $L"'
            % (stamp, stamp, b64(script), b64(script))
        )
    else:
        script_b64 = b64(script)
        launcher = (
            "$b = %s; $d = Join-Path ([IO.Path]::GetTempPath()) 'remote-job-%s';"
            "New-Item -ItemType Directory -Force -Path $d | Out-Null;"
            "$s = Join-Path $d 'job.ps1'; $l = Join-Path $d 'job.log';"
            "[IO.File]::WriteAllText($s, [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('%s')), (New-Object Text.UTF8Encoding $false));"
            "$p = Start-Process -FilePath (Get-Process -Id $PID).Path "
            "-ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',$s) "
            "-RedirectStandardOutput $l -RedirectStandardError ($l + '.err') -PassThru;"
            "Write-Output ('{0} {1}' -f $p.Id, $l)"
            % (powershell_quote(stamp), stamp, script_b64)
        )
    result = execute("", target, shell, wrap(launcher, shell), timeout, 64 * 1024, prefer_session=False)
    text = result["stdout"].decode("utf-8", "replace").strip()
    if result["exit"] not in (0, None) or not text:
        raise RemoteError("could not start the job: %s" % (result["stderr"].decode("utf-8", "replace").strip() or "exit %s" % result["exit"]))
    pid, _, log = text.partition(" ")
    return {"pid": pid, "log": log, "ms": result["ms"]}


# --- doctor --------------------------------------------------------------------


def out(line=""):
    print(line, flush=True)


def check_line(ok, label, detail=""):
    out("  %s %-34s %s" % ("ok  " if ok else "FAIL", label, detail))
    return 0 if ok else 1


def doctor(name, target):
    failures = 0
    out("target            %s (%s)" % (name, destination(target)))
    out("port              %s" % target["port"])
    out("identity          %s" % (target.get("identity") or "ssh defaults"))
    ssh = shutil.which("ssh")
    failures += check_line(bool(ssh), "ssh on PATH", ssh or "not found")
    failures += check_line(bool(shutil.which("sftp")), "sftp on PATH", shutil.which("sftp") or "not found")
    if not ssh:
        out("")
        out("verdict           CANNOT RUN")
        return 1
    version = subprocess.run([ssh, "-V"], capture_output=True, text=True).stderr.strip()
    out("ssh version       %s" % version)

    shell = target["shell"]
    out("shell             %s" % shell)
    out("login shell       %s" % (target.get("login") or "?"))
    if target.get("runner"):
        out("runner            %s" % target["runner"])

    probe = "echo __probe__"
    result = run_oneshot(target, shell, wrap(probe, shell), 25, 64 * 1024)
    reachable = result["exit"] == 0 and b"__probe__" in result["stdout"]
    failures += check_line(reachable, "one-shot round trip", "%d ms" % result["ms"] if reachable else (result["stderr"].decode("utf-8", "replace").strip().splitlines() or ["no answer"])[-1])

    if reachable:
        # Exit codes: a bare remote failure reports 1, so the number has to be proved.
        code = run_oneshot(target, shell, wrap("exit 7", shell), 25, 4096)
        failures += check_line(code["exit"] == 7, "exit code fidelity", "exit 7 came back as %s" % code["exit"])
        # Text: the GBK trap. A Chinese string must survive as UTF-8 bytes.
        text = run_oneshot(target, shell, wrap("echo '远程 ✓'", shell), 25, 4096)
        wanted = "远程 ✓".encode("utf-8")
        failures += check_line(wanted in text["stdout"], "UTF-8 round trip", repr(text["stdout"][:24]) if wanted not in text["stdout"] else "exact")
        # Bytes: framing must not be text-shaped anywhere in the path.
        if shell == POSIX:
            binary = "printf '\\000\\001\\037\\177\\200\\377'"
        else:
            binary = "[Console]::OpenStandardOutput().Write([byte[]](0,1,31,127,128,255),0,6)"
        blob = run_oneshot(target, shell, wrap(binary, shell), 25, 4096)
        failures += check_line(blob["stdout"] == bytes([0, 1, 31, 127, 128, 255]), "binary round trip", repr(blob["stdout"][:8]))

    # Transfer: a name with a space is the case that silently mis-parses.
    local = os.path.join(tempfile.gettempdir(), "remote doctor %s.txt" % uuid.uuid4().hex[:6])
    remote_path = None
    try:
        if shell == POSIX:
            probe_dir = run_oneshot(target, shell, wrap('printf %s "${TMPDIR:-/tmp}"', shell), 25, 4096)
            base_dir = probe_dir["stdout"].decode("utf-8", "replace").strip() or "/tmp"
            remote_path = "%s/remote-doctor-%s.txt" % (base_dir.rstrip("/"), uuid.uuid4().hex[:6])
        else:
            probe_dir = run_oneshot(target, shell, wrap("[IO.Path]::GetTempPath()", shell), 25, 4096)
            base_dir = probe_dir["stdout"].decode("utf-8", "replace").strip()
            remote_path = os.path.join(base_dir.rstrip("\\/") if base_dir else ".", "remote-doctor-%s.txt" % uuid.uuid4().hex[:6])
        with open(local, "w", encoding="utf-8") as handle:
            handle.write("远程 12345\n")
        info = put_file(target, shell, local, remote_path, timeout=60, verify=True)
        failures += check_line(True, "put verifies size", "%d bytes" % info["bytes"])
        back = local + ".back"
        get_file(target, shell, remote_path, back, timeout=60, verify=True)
        same = open(back, "rb").read() == open(local, "rb").read()
        failures += check_line(same, "get returns the same bytes")
        os.unlink(back)
    except (RemoteError, OSError) as error:
        failures += check_line(False, "transfer round trip", str(error))
    finally:
        try:
            os.unlink(local)
        except OSError:
            pass

    possible, why = session_possible(target)
    if not possible:
        out("session           not used: %s" % why)
        out("")
        out("verdict           %s" % ("READY" if not failures else "%d CHECK(S) FAILED" % failures))
        return 0 if not failures else 1

    # The session: prove it is faster and that it agrees with the one-shot.
    try:
        state = ensure_daemon(name)
        round_trips = []
        for _ in range(5):
            started = time.time()
            reply = call_daemon(state["port"], {"cmd": "run", "script": wrap("echo __probe__", shell), "timeout": 20, "max_output": 65536}, 20)
            round_trips.append((time.time() - started) * 1000)
            if not reply.get("ok") or b"__probe__" not in decode_result(reply)["stdout"]:
                raise RemoteError(reply.get("error") or "the session answered without the probe")
        round_trips.sort()
        failures += check_line(True, "session round trip", "median %d ms over 5" % round_trips[len(round_trips) // 2])
        code = call_daemon(state["port"], {"cmd": "run", "script": wrap("exit 7", shell), "timeout": 20, "max_output": 4096}, 20)
        failures += check_line(decode_result(code)["exit"] == 7, "session exit code fidelity")
        out("session port      %s (pid %s)" % (state["port"], state.get("pid")))
    except RemoteError as error:
        failures += check_line(False, "session", str(error))

    out("")
    out("verdict           %s" % ("READY" if not failures else "%d CHECK(S) FAILED" % failures))
    return 0 if not failures else 1


# --- command line --------------------------------------------------------------


def target_or_die(name):
    targets = load_targets()
    if name not in targets:
        raise RemoteError(
            "no target named %r; add one with: remote add %s --host HOST --user USER" % (name, name)
        )
    return normalize_target(targets[name])


def read_script(args):
    if getattr(args, "file", None):
        with open(args.file, "rb") as handle:
            return handle.read().decode("utf-8")
    if args.script == "-" or (args.script is None and not sys.stdin.isatty()):
        data = sys.stdin.buffer.read()
        if data:
            return data.decode("utf-8")
    if args.script is None:
        raise RemoteError("nothing to run: pass a script, --file PATH, or pipe one on stdin")
    return args.script


def emit_result(result, name):
    sys.stdout.buffer.write(result.get("stdout") or b"")
    sys.stdout.buffer.flush()
    sys.stderr.buffer.write(result.get("stderr") or b"")
    sys.stderr.buffer.flush()
    moved = result.get("ms")
    notes = []
    if result.get("truncated"):
        notes.append("output truncated")
    if result.get("timedOut"):
        notes.append("timed out on the client; the remote may still be running it")
    if result.get("outcomeUnknown"):
        notes.append("outcome unknown, not replayed")
    if notes:
        sys.stderr.write("remote: %s\n" % "; ".join(notes))
    if os.environ.get("REMOTE_TRACE"):
        sys.stderr.write("remote: %s %s ms\n" % (result.get("path"), moved))
    if result.get("exit") is None:
        return 124
    return int(result["exit"])


def cmd_add(args):
    targets = load_targets()
    record = {
        "host": args.host,
        "port": args.port,
        "user": args.user,
        "identity": args.identity,
        "shell": args.shell,
        "runner": args.runner,
        "login": args.login,
        "strict": args.strict,
        "jump": args.jump,
        "no_config": args.no_config,
        "connect_timeout": 10,
    }
    targets[args.name] = normalize_target(record)
    save_targets(targets)
    out("%s -> %s" % (args.name, destination(targets[args.name])))
    out("stored in         %s" % TARGETS_PATH)
    return 0


def cmd_ls(args):
    targets = load_targets()
    if not targets:
        out("no targets yet; add one with: remote add NAME --host HOST --user USER")
        return 0
    for name in sorted(targets):
        target = normalize_target(targets[name])
        session = daemon_alive(name)
        marks = []
        if target.get("identity"):
            marks.append("key " + os.path.basename(target["identity"]))
        if target.get("jump"):
            marks.append("via " + target["jump"])
        marks.append("session up" if session else "no session")
        out("  %-12s %-34s %-8s %s" % (name, destination(target), target["shell"], ", ".join(marks)))
    return 0


def cmd_session(args):
    if args.action == "status":
        names = [args.name] if args.name else sorted(load_targets())
    elif not args.name:
        raise RemoteError("session %s needs a target name" % args.action)
    else:
        names = [args.name]
    if args.action == "start":
        state = ensure_daemon(args.name)
        out("%s ready on 127.0.0.1:%s (pid %s, shell %s)" % (args.name, state["port"], state.get("pid"), state.get("shell")))
        return 0
    if args.action == "stop":
        state = daemon_alive(args.name)
        if not state:
            out("%s has no session" % args.name)
            return 0
        try:
            call_daemon(state["port"], {"cmd": "shutdown"}, 10)
        except RemoteError:
            pass
        clear_session_state(args.name)
        out("%s stopped" % args.name)
        return 0
    for name in names:
        state = daemon_alive(name)
        if state:
            ping = {}
            try:
                ping = call_daemon(state["port"], {"cmd": "ping"}, 10)
            except RemoteError:
                pass
            out("  %-12s 127.0.0.1:%-6s pid %-7s shell %-11s opened %s" % (
                name, state.get("port"), state.get("pid"), state.get("shell"), ping.get("opened", "?")))
        else:
            out("  %-12s no session" % name)
    return 0


def cmd_put(args):
    target = resolve(args.name, target_or_die(args.name), args.shell)
    info = put_file(target, target["shell"], args.local, args.remote, args.timeout, not args.no_verify, args.mode)
    out("%s -> %s:%s  %d bytes%s" % (
        args.local, args.name, args.remote, info["bytes"],
        " (verified)" if info.get("verified") else ""))
    return 0


def cmd_get(args):
    target = resolve(args.name, target_or_die(args.name), args.shell)
    info = get_file(target, target["shell"], args.remote, args.local, args.timeout, not args.no_verify)
    out("%s:%s -> %s  %d bytes%s" % (
        args.name, args.remote, args.local, info["bytes"],
        " (verified)" if info.get("verified") else ""))
    return 0


def build_parser():
    parser = argparse.ArgumentParser(
        prog="remote",
        description="run commands and move files on a remote machine, without local conventions",
    )
    sub = parser.add_subparsers(dest="cmd")

    p = sub.add_parser("add", help="remember a target")
    p.add_argument("name")
    p.add_argument("--host", required=True)
    p.add_argument("--port", type=int, default=22)
    p.add_argument("--user")
    p.add_argument("--identity", help="private key file; omit to use ssh's own defaults")
    p.add_argument("--shell", default="auto", choices=["auto", POSIX, POWERSHELL])
    p.add_argument("--runner", help="interpreter prefix for posix scripts, e.g. 'C:\\Git\\bin\\bash.exe'")
    p.add_argument("--login", default="auto", choices=["auto", POSIX, POWERSHELL, "cmd"],
                   help="the shell sshd runs your command in (default: detect)")
    p.add_argument("--strict", default="accept-new", choices=["yes", "no", "accept-new"])
    p.add_argument("--jump", help="ProxyJump host")
    p.add_argument("--no-config", action="store_true", help="pass -F /dev/null for a hermetic connection")

    sub.add_parser("ls", help="list targets and their sessions")

    p = sub.add_parser("rm", help="forget a target")
    p.add_argument("name")

    p = sub.add_parser("doctor", help="prove the link works, and how fast")
    p.add_argument("name")

    p = sub.add_parser("run", help="run a script on the target")
    p.add_argument("name")
    p.add_argument("script", nargs="?", help="script text; '-' or omit to read stdin")
    p.add_argument("--file", help="read the script from a local file")
    p.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT)
    p.add_argument("--cwd")
    p.add_argument("--env", action="append", default=[], metavar="K=V")
    p.add_argument("--max-output", type=int, default=DEFAULT_MAX_OUTPUT)
    p.add_argument("--direct", action="store_true", help="one ssh, no session")
    p.add_argument("--detach", action="store_true", help="start it in the background")
    p.add_argument("--shell", choices=[POSIX, POWERSHELL])

    p = sub.add_parser("put", help="copy a local file to the target")
    p.add_argument("name")
    p.add_argument("local")
    p.add_argument("remote")
    p.add_argument("--mode", help="chmod the result")
    p.add_argument("--no-verify", action="store_true")
    p.add_argument("--timeout", type=float, default=300.0)
    p.add_argument("--shell", choices=[POSIX, POWERSHELL])

    p = sub.add_parser("get", help="copy a remote file here")
    p.add_argument("name")
    p.add_argument("remote")
    p.add_argument("local")
    p.add_argument("--no-verify", action="store_true")
    p.add_argument("--timeout", type=float, default=300.0)
    p.add_argument("--shell", choices=[POSIX, POWERSHELL])

    p = sub.add_parser("session", help="inspect or control the persistent session")
    p.add_argument("action", choices=["start", "stop", "status"])
    p.add_argument("name", nargs="?")

    p = sub.add_parser("_daemon", help=argparse.SUPPRESS)
    p.add_argument("name")
    return parser


def parse_env(pairs):
    env = {}
    for pair in pairs or []:
        key, separator, value = pair.partition("=")
        if not key or not separator:
            raise RemoteError("--env needs K=V, got %r" % pair)
        env[key.strip()] = value
    return env


def main(argv):
    parser = build_parser()
    args = parser.parse_args(argv)
    if not args.cmd:
        parser.print_help()
        return 0

    if args.cmd == "_daemon":
        return serve(args.name)
    if args.cmd == "add":
        return cmd_add(args)
    if args.cmd == "ls":
        return cmd_ls(args)
    if args.cmd == "rm":
        targets = load_targets()
        if args.name in targets:
            targets.pop(args.name)
            save_targets(targets)
            clear_session_state(args.name)
            out("%s forgotten" % args.name)
        else:
            raise RemoteError("no target named %r" % args.name)
        return 0
    if args.cmd == "session":
        return cmd_session(args)
    if args.cmd == "put":
        return cmd_put(args)
    if args.cmd == "get":
        return cmd_get(args)

    target = resolve(args.name, target_or_die(args.name), getattr(args, "shell", None))
    shell = target["shell"]
    if args.cmd == "doctor":
        return doctor(args.name, target)
    if args.cmd == "run":
        script = read_script(args)
        if args.detach:
            info = detach(target, shell, script, args.timeout)
            out("started           pid %s" % info["pid"])
            out("log               %s" % info["log"])
            # Print the command a person can copy, quoting only the path: quoting the
            # whole line produced nested-quote soup that nobody would type.
            if shell == POSIX:
                watch = "tail -n 20 %s" % posix_quote(info["log"])
                shown = "remote run %s %s" % (args.name, posix_quote(watch))
            else:
                watch = "Get-Content -LiteralPath %s -Tail 20" % info["log"]
                shown = 'remote run %s "%s"' % (args.name, watch)
            out("watch with        %s" % shown)
            return 0
        wrapped = wrap(script, shell, args.cwd, parse_env(args.env))
        result = execute(args.name, target, shell, wrapped, args.timeout, args.max_output, not args.direct)
        return emit_result(result, args.name)
    parser.print_help()
    return 2


def reader_went_away(error):
    if isinstance(error, BrokenPipeError):
        return True
    return isinstance(error, OSError) and getattr(error, "errno", None) == 22


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except RemoteError as error:
        print("remote: %s" % error, file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
    except OSError as error:
        if not reader_went_away(error):
            raise
        try:
            sink = os.open(os.devnull, os.O_WRONLY)
            os.dup2(sink, sys.stdout.fileno())
        except OSError:
            pass
        sys.exit(0)
