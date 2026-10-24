#!/usr/bin/env python3
"""Tests for the remote transport, without a network.

Two layers:

  * pure functions -- target defaults, ssh flags, quoting, script wrapping. These are
    where a wrong default silently sends a command somewhere else, or turns an exit
    code into 1.
  * a session against a fake login shell (`fake_login_shell.py`) that runs the command
    locally in bash. That reproduces the two facts a session depends on -- a shell gets
    a command string, and a channel carries stdin/stdout -- so the framing, the exit
    codes and the behaviour on a second request are exercised for real.

What is deliberately not faked is the daemon: its wiring is the same code path as the
in-process session, and its correctness was measured against a live host instead.

    python3 test_remote.py        # exits non-zero on failure
"""
import importlib.util
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("remote", os.path.join(HERE, "remote.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

bad = 0


def check(name, condition, detail=""):
    global bad
    if condition:
        print("  ok   %s" % name)
    else:
        bad += 1
        print("  FAIL %s%s" % (name, ("  -- " + str(detail)) if detail else ""))


def raises(name, function, fragment=""):
    global bad
    try:
        function()
    except m.RemoteError as error:
        if fragment and fragment not in str(error):
            bad += 1
            print("  FAIL %s  -- message was %r" % (name, str(error)))
        else:
            print("  ok   %s" % name)
        return
    bad += 1
    print("  FAIL %s  -- nothing was raised" % name)


print("target defaults")
target = m.normalize_target({"host": "h"})
check("port defaults to 22", target["port"] == 22)
check("user has no guessed default", target["user"] is None, target["user"])
check("shell starts unknown", target["shell"] == "auto")
raises("a target needs a host", lambda: m.normalize_target({}), "host")
raises("unknown shell refused", lambda: m.normalize_target({"host": "h", "shell": "fish"}))
raises("runner with powershell refused", lambda: m.normalize_target({"host": "h", "shell": "powershell", "runner": "bash"}))
check("destination without user", m.destination({"host": "h", "user": None}) == "h")
check("destination with user", m.destination({"host": "h", "user": "u"}) == "u@h")

print("ssh flags are built from the target, not from local files")
argv = m.ssh_base(m.normalize_target({"host": "h", "user": "u", "identity": "~/.ssh/k", "port": 2200, "jump": "j"}), interactive_stdin=True)
text = " ".join(argv)
check("a session keeps stdin as the channel", "-n" not in argv)
check("BatchMode on", "BatchMode=yes" in text)
check("port passed", "-p 2200" in text)
check("identity passed", "-i" in text and ".ssh/k" in text)
check("jump passed", "-J j" in text)
check("one-shot keeps stdin away", "-n" in m.ssh_base(m.normalize_target({"host": "h"})))
check("the default is the one-shot shape", "-n" in m.ssh_base(m.normalize_target({"host": "h"})))
check("no -F unless asked", "-F" not in text)
check("hermetic when asked", "-F /dev/null" in " ".join(m.ssh_base(m.normalize_target({"host": "h", "no_config": True}))))

print("quoting")
check("posix quote of a plain word", m.posix_quote("abc") == "'abc'")
check("posix quote survives a quote char", m.posix_quote("it's") == "'it'\\''s'")
check("powershell quote doubles", m.powershell_quote("it's") == "'it''s'")
check("sftp leaves a simple path alone", m.sftp_quote("/tmp/a.txt") == "/tmp/a.txt")
check("sftp quotes a space", m.sftp_quote("/tmp/a b.txt") == '"/tmp/a b.txt"')
# sftp has its own parser: inside double quotes a backslash must be doubled, or the
# Windows path lands somewhere else without any error.
check("sftp doubles backslashes", m.sftp_quote("C:\\Users\\a b") == '"C:\\\\Users\\\\a b"', m.sftp_quote("C:\\Users\\a b"))

print("script wrapping")
posix = m.wrap_posix("echo hi", cwd="/w", env={"K": "v"})
check("posix exports env", "export K='v'" in posix)
check("posix changes directory", "cd '/w'" in posix)
check("posix keeps the script", "echo hi" in posix)
ps = m.wrap_powershell("Write-Output hi", cwd="C:\\w")
check("powershell re-raises the exit code", "exit $LASTEXITCODE" in ps)
check("powershell sets a stop preference", "$ErrorActionPreference = 'Stop'" in ps)
check("powershell sets UTF-8 output", "UTF8Encoding" in ps)

print("login shell trailers")
check("posix needs no trailer", m.LOGIN_TRAILERS[m.POSIX] == "")
check("powershell trailer re-raises", "LASTEXITCODE" in m.LOGIN_TRAILERS[m.POWERSHELL])
check("cmd trailer uses ERRORLEVEL", "ERRORLEVEL" in m.LOGIN_TRAILERS["cmd"])
check("ssh_command appends the trailer", m.ssh_command({"login": m.POWERSHELL}, "X").endswith("; exit $LASTEXITCODE"))

print("env parsing")
check("K=V", m.parse_env(["A=1"]) == {"A": "1"})
check("value may hold =", m.parse_env(["A=b=c"]) == {"A": "b=c"})
raises("a bare word is refused", lambda: m.parse_env(["novalue"]))

print("session gate")
check("a runner target has no session", m.session_possible({"runner": "bash"})[0] is False)
check("an ordinary target may have one", m.session_possible({})[0] is True)

print("\nPOSIX session against a fake login shell")
FAKE = os.path.join(HERE, "fake_login_shell.py")
real_ssh_base = m.ssh_base
m.ssh_base = lambda target, interactive_stdin=False: [sys.executable, FAKE]
shell = m.POSIX
target = m.normalize_target({"host": "fake", "shell": shell, "login": m.POSIX})
session = m.Session(target, shell)
session.open()
try:
    first = session.run(m.wrap("printf 'one\\n'", shell), 20, 1 << 20)
    check("first request answers", first["stdout"] == b"one\n", first["stdout"])
    check("first request reports 0", first["exit"] == 0)

    # This is the case that matters: a request arriving after the loop has gone idle.
    # The original design failed only on the second request, and every symptom pointed
    # at ssh rather than at the loop.
    time.sleep(1.5)
    second = session.run(m.wrap("printf 'two\\n'", shell), 20, 1 << 20)
    check("a later request still answers", second["stdout"] == b"two\n", second["stdout"])

    exit7 = session.run(m.wrap("exit 7", shell), 20, 4096)
    check("exit code survives the session", exit7["exit"] == 7, exit7["exit"])

    text = session.run(m.wrap("printf '远程\\n'", shell), 20, 4096)
    check("UTF-8 survives the session", text["stdout"] == "远程\n".encode("utf-8"), text["stdout"])

    # Binary must not be text-shaped anywhere in the framing.
    blob = session.run(m.wrap("printf '\\000\\001\\037\\177\\200\\377'", shell), 20, 4096)
    check("binary survives the session", blob["stdout"] == bytes([0, 1, 31, 127, 128, 255]), blob["stdout"])

    big = session.run(m.wrap("head -c 200000 /dev/zero | tr '\\0' 'x'", shell), 30, 1 << 20)
    check("output larger than one chunk is reassembled", len(big["stdout"]) == 200000, len(big["stdout"]))

    cut = session.run(m.wrap("head -c 5000 /dev/zero | tr '\\0' 'y'", shell), 30, 1000)
    check("max-output truncates", len(cut["stdout"]) == 1000 and cut["truncated"] is True, (len(cut["stdout"]), cut["truncated"]))

    slow = session.run(m.wrap("sleep 30", shell), 2, 4096)
    check("a timeout is reported, not waited out", slow["timedOut"] is True)
    check("a timed-out outcome is marked unknown", slow["outcomeUnknown"] is True)
finally:
    session.close()
    m.ssh_base = real_ssh_base

print("\nrestart after a timeout")
m.ssh_base = lambda target, interactive_stdin=False: [sys.executable, FAKE]
session = m.Session(target, shell)
try:
    session.open()
    session.run(m.wrap("sleep 30", shell), 2, 4096)
    # A timed-out request leaves unread frames behind. Without per-request ids the next
    # reply would be attributed to the wrong command.
    fresh = session.run(m.wrap("printf 'after-timeout\\n'", shell), 20, 4096)
    check("the next call gets its own reply", fresh["stdout"] == b"after-timeout\n", fresh["stdout"])
finally:
    session.close()
    m.ssh_base = real_ssh_base

print("\nexecute honours the session gate")
recorded = []


def fake_oneshot(target, shell, script_text, timeout, max_output):
    recorded.append("oneshot")
    return {"stdout": b"ok", "stderr": b"", "exit": 0, "ms": 1, "truncated": False,
            "timedOut": False, "outcomeUnknown": False}


def fake_ensure_daemon(name):
    recorded.append("session")
    raise m.RemoteError("a session must not be attempted for this target")


real_oneshot, real_ensure = m.run_oneshot, m.ensure_daemon
m.run_oneshot, m.ensure_daemon = fake_oneshot, fake_ensure_daemon
try:
    runner_target = m.normalize_target({"host": "h", "shell": m.POSIX, "login": m.POWERSHELL, "runner": "bash"})
    try:
        m.execute("h", runner_target, m.POSIX, "echo ok", 5, 4096, prefer_session=True)
    except m.RemoteError:
        pass  # a session being attempted at all is the failure this asserts against
    check("a runner target falls back to one-shot", recorded == ["oneshot"], recorded)

    recorded.clear()
    plain = m.normalize_target({"host": "h", "shell": m.POSIX, "login": m.POSIX})
    try:
        m.execute("h", plain, m.POSIX, "echo ok", 5, 4096, prefer_session=True)
    except m.RemoteError:
        pass
    check("an ordinary target does try the session", recorded == ["session"], recorded)
finally:
    m.run_oneshot, m.ensure_daemon = real_oneshot, real_ensure

print("\nframing ignores another request's reply")
m.ssh_base = lambda target, interactive_stdin=False: [sys.executable, FAKE]
session = m.Session(target, shell)
try:
    session.open()
    # A reply that belongs to a request we already gave up on can still be in flight.
    # Without per-request ids it would be read as this request's answer -- the wrong
    # output, attributed to the wrong command, with no error anywhere.
    with session.lock:
        session.frames.append("O deadbeef Zm9yZWlnbg==")
        session.frames.append("X deadbeef 0")
    real = session.run(m.wrap("printf 'mine\\n'", shell), 20, 4096)
    check("a foreign reply is not taken as the answer", real["stdout"] == b"mine\n", real["stdout"])
    check("a foreign exit code is not taken either", real["exit"] == 0, real["exit"])
finally:
    session.close()
    m.ssh_base = real_ssh_base

print("\ntransfers against a fake sftp")
import tempfile

FAKE_SFTP = os.path.join(HERE, "fake_sftp.py")
real_sftp_base, real_run_oneshot = m.sftp_base, m.run_oneshot


# The fake sftp speaks the real batch protocol, so quoting and the size check are
# exercised rather than mocked away.
m.sftp_base = lambda target: [sys.executable, FAKE_SFTP]


def fake_oneshot_for_size(target, shell, script_text, timeout, max_output):
    """Stand in for the size query: report the local file's size for the asked path."""
    import re
    match = re.search(r"'([^']+)'", script_text)
    path = match.group(1) if match else ""
    size = os.path.getsize(path) if os.path.isfile(path) else 0
    return {"stdout": ("%d\n" % size).encode(), "stderr": b"", "exit": 0, "ms": 1,
            "truncated": False, "timedOut": False, "outcomeUnknown": False}


m.run_oneshot = fake_oneshot_for_size
try:
    workdir = tempfile.mkdtemp()
    local = os.path.join(workdir, "a file with spaces.txt")
    with open(local, "w", encoding="utf-8") as handle:
        handle.write("远程 ✓ 12345\n")
    remote_copy = os.path.join(workdir, "b file with spaces.txt")
    info = m.put_file(None, m.POSIX, local, remote_copy, verify=True)
    check("put reports the size it verified", info["bytes"] == os.path.getsize(local), info)
    check("put landed the bytes", open(remote_copy, "rb").read() == open(local, "rb").read())

    back = os.path.join(workdir, "back.txt")
    m.get_file(None, m.POSIX, remote_copy, back, verify=True)
    check("get returns the same bytes", open(back, "rb").read() == open(local, "rb").read())

    # A truncated copy is the failure this whole check exists for: sftp reported success
    # and the file was short. It has to be an error, not a note.
    def short_size(target, shell, script_text, timeout, max_output):
        return {"stdout": b"999\n", "stderr": b"", "exit": 0, "ms": 1,
                "truncated": False, "timedOut": False, "outcomeUnknown": False}

    m.run_oneshot = short_size
    raises("a short copy is an error, not a note",
           lambda: m.put_file(None, m.POSIX, local, os.path.join(workdir, "c.txt"), verify=True),
           "did not arrive intact")
finally:
    m.sftp_base, m.run_oneshot = real_sftp_base, real_run_oneshot

print("\ncommand-line budget")
over = "x" * 40000
raises(
    "an oversized one-shot is refused with a way out",
    lambda: m.run_oneshot(m.normalize_target({"host": "fake", "shell": m.POSIX, "login": m.POSIX}), m.POSIX, over, 10, 4096),
    "put",
)

print("\n%s" % ("PASS" if not bad else "%d FAILED" % bad))
sys.exit(1 if bad else 0)
