#!/usr/bin/env python3
"""Tests for polity.py, without a server and without a model.

Two layers, split where the mistakes actually live:

  * `wait_loop` against a scripted clock. The dangerous case is not protocol, it is
    time: "idle" is also the state the session was left in by the turn *before* the
    one we dispatched, so a wait that only looks at status returns instantly with
    the wrong answer.
  * the plumbing against `fake_cli.py` through real subprocesses, because polity.py
    starts a new client for every command and parses what it prints. The baseline
    being read *before* the send is checked here by call order, not by comment.

    python3 test_polity.py        # exits non-zero on failure
"""
import contextlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("polity", os.path.join(HERE, "polity.py"))
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


def equal(name, got, want):
    check(name, got == want, "got %r want %r" % (got, want))


def args_of(argv):
    out = {}
    i = 0
    while i < len(argv):
        if argv[i].startswith("--"):
            out[argv[i][2:]] = argv[i + 1] if i + 1 < len(argv) else ""
            i += 2
        else:
            i += 1
    return out


def row(seq, role, text, kind="text"):
    return {"seq": seq, "kind": kind, "role": role, "text": text}


def _parse_seq(raw, last):
    """Same shape the server accepts: `<a>:<b>`, inclusive, 1-based, negative
    counting back from the newest row."""
    start_s, _, end_s = str(raw).partition(":")

    def one(text, default):
        if text == "":
            return default
        n = int(text)
        if n == 0:
            raise ValueError("--seq counts from 1, or from -1 backwards")
        return last + 1 + n if n < 0 else n

    return one(start_s, 1), one(end_s, last)


class Clock:
    """A scripted session: one `(status, rows)` step per poll, and time that moves
    only when waited on, so a timeout is reachable without sleeping."""

    def __init__(self, steps):
        self.steps = list(steps)
        self.n = 0
        self.now = 0.0
        self.slept = []

    def _step(self):
        return self.steps[min(self.n, len(self.steps) - 1)]

    def status_of(self, session, probe):
        return self._step()[0], {}

    def rows(self, session, after):
        status, all_rows = self._step()
        self.n += 1
        return [r for r in all_rows if r["seq"] > after], (
            all_rows[-1]["seq"] if all_rows else 0
        )

    def sleep(self, seconds):
        self.slept.append(seconds)
        self.now += seconds

    def clock(self):
        return self.now


def run_wait(clock, after, timeout=5, poll=1.0):
    real = m.status_of
    m.status_of = clock.status_of
    try:
        return m.wait_loop(
            "w", after, timeout, "w", poll,
            sleep=clock.sleep, tail=clock.rows, clock=clock.clock,
        )
    finally:
        m.status_of = real


class Server:
    """run_cli's stand-in: records the argv it was given and answers like the client."""

    def __init__(self, replies=(), statuses=None):
        self.calls = []
        self.sessions = {}
        self.reply = replies
        self.statuses = dict(statuses or {})

    def create(self, name, rows=(), created_at=1792837000000):
        self.sessions[name] = {"rows": list(rows), "created_at": created_at}

    def __call__(self, args, timeout=120):
        self.calls.append(list(args))
        name = args[0]
        a = args_of(args[1:])
        if name == "set_session":
            s = a.get("session")
            self.sessions.setdefault(s, {"rows": [], "created_at": 1792837000000})
            live = self.sessions[s]
            evs = [
                {"t": "sessions", "sessions": [
                    {"name": k, "status": self.statuses.get(k, "idle"),
                     "created_at": v["created_at"], "messages": len(v["rows"])}
                    for k, v in self.sessions.items()
                ]},
                {"t": "cmd_result", "id": 1, "ok": True,
                 "message": "session is now %s (llm fake/model)" % s,
                 "data": {"session": s, "created_at": live["created_at"],
                          "llm": "fake/model"}},
            ]
            return 0, evs, ""
        if name == "get_messages":
            s = a.get("session")
            live = self.sessions.get(s)
            if live is None:
                return 0, [{"t": "cmd_result", "id": 1, "ok": False,
                            "message": "no session named %r" % s}], ""
            for stale in ("after_seq", "limit"):
                if stale in a:
                    return 0, [{"t": "cmd_result", "id": 1, "ok": False,
                                "message": "get_messages has no argument --%s" % stale}], ""
            last = len(live["rows"])
            first, end = _parse_seq(a.get("seq"), last)
            rows = [r for r in live["rows"] if first <= r["seq"] <= end]
            return 0, [{"t": "cmd_result", "id": 1, "ok": True,
                        "message": "%d row(s)" % len(rows),
                        "data": {"last_seq": last, "messages": rows,
                                 "session": s}}], ""
        if name == "send_message":
            s = a.get("session")
            live = self.sessions.get(s)
            if live is None:
                return 0, [{"t": "cmd_result", "id": 1, "ok": False,
                            "message": "no such session"}], ""
            live["rows"].append(row(len(live["rows"]) + 1, "user", a.get("message", "")))
            for r in self.reply:
                live["rows"].append(dict(r, seq=len(live["rows"]) + 1))
            return 0, [{"t": "cmd_result", "id": 1, "ok": True,
                        "message": "message accepted by %s" % s}], ""
        return 0, [{"t": "cmd_result", "id": 1, "ok": False,
                    "message": "unknown command"}], ""


@contextlib.contextmanager
def server_installed(server):
    real = m.run_cli
    m.run_cli = server
    try:
        yield server
    finally:
        m.run_cli = real


@contextlib.contextmanager
def quiet():
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        yield out, err


def run(*argv):
    return m.main(list(argv))


# ---------------------------------------------------------------- 解析
print("\nparsing what the client prints")
evs = m.parse_events(
    'gongga · offline — connecting…\n{"t":"cmd","id":1}\n'
    'oops\n{"t":"cmd_result","id":1,"ok":true,"message":"first"}\n'
    '{"t":"cmd_result","id":2,"ok":true,"message":"second"}\n'
)
equal("the banner line is not an event", len(evs), 3)
equal("the last cmd_result wins", m.cmd_result(evs)["message"], "second")
check("a malformed line is skipped, not fatal",
      m.parse_events('{"t":"cmd","id":1}\n{"t": broken}\n') == [{"t": "cmd", "id": 1}])
# A line can be valid JSON and still not an event: `42`, `"x"`, `null`. Keeping one
# would put a non-dict into the stream and `e.get` would blow up later.
check("a JSON scalar line is not an event",
      m.parse_events('42\nnull\n"x"\n{"t":"cmd","id":1}\n') == [{"t": "cmd", "id": 1}])
check("and the stream still answers", m.cmd_result(m.parse_events('42\n{"t":"cmd_result","ok":true}\n')) is not None)
check("nothing parsed reads as failure", m.result_of([], "x")[0] is False)
# A client that never started must not be reported as a client that said nothing.
check("the client's stderr is passed through",
      "no client at" in m.result_of([], "x", "no client at '/nope'")[1],
      m.result_of([], "x", "no client at '/nope'")[1])

saved = os.environ.get("GONGGA_CLI")
os.environ["GONGGA_CLI"] = os.path.join(tempfile.gettempdir(), "polity-no-such-client")
try:
    code, evs, err = m.run_cli(["status"])
    check("a missing client is a CLI failure, not silence", code == m.EXIT_CLI, code)
    check("and it names the path", "polity-no-such-client" in err, err)
finally:
    if saved is None:
        os.environ.pop("GONGGA_CLI", None)
    else:
        os.environ["GONGGA_CLI"] = saved

# ---------------------------------------------------------------- wait
print("\nwait: the state left over from the turn before is not an answer")
old = [row(1, "user", "previous task"), row(2, "assistant", "DONE 000")]
c = Clock([("idle", old)])
code, final, note = run_wait(c, after=2, timeout=5, poll=1.0)
equal("a stale idle times out instead of returning the old reply", code, m.EXIT_TIMEOUT)
check("and it says why", "超时" in note, note)
check("it kept polling", len(c.slept) >= 4, c.slept)

c = Clock([("running", old), ("running", old), ("idle", old + [row(3, "assistant", "DONE 001")])])
code, final, note = run_wait(c, after=2)
equal("running then idle with a new reply is a finish", code, m.EXIT_OK)
equal("the reply is the new one", final["text"], "DONE 001")

c = Clock([("idle", old + [row(3, "assistant", "DONE 002")])])
code, final, _ = run_wait(c, after=2)
equal("a turn that is already over is still readable", code, m.EXIT_OK)
equal("with its reply", final["text"], "DONE 002")

# The reply row lands just before the state flips: a wait that trusts the row alone
# hands back a turn that is still running.
late = old + [row(3, "assistant", "DONE 003")]
c = Clock([("running", late)])
code, final, note = run_wait(c, after=2, timeout=3, poll=1.0)
equal("a reply while still running is not a finish", code, m.EXIT_TIMEOUT)
check("and nothing was handed back", final is None, final)

c = Clock([("running", old + [row(3, "tool_call", '{"command":"sleep 9"}')]), ("idle", old + [row(3, "tool_call", "{}")])])
code, final, note = run_wait(c, after=2)
equal("a turn that ends without closing prose is not success", code, m.EXIT_NO_REPLY)
check("it says so", "没有收尾" in note, note)

c = Clock([("running", old + [row(3, "user", "x")])])
code, final, note = run_wait(c, after=2, timeout=3, poll=1.0)
equal("still running at the deadline times out", code, m.EXIT_TIMEOUT)
check("the note blames the turn, not the dispatch", "回合可能还在跑" in note, note)

c = Clock([("idle", old)])
code, final, note = run_wait(c, after=2, timeout=3, poll=1.0)
check("no rows past the baseline is called out as a dispatch that did not land",
      "没落进去" in note, note)

with quiet() as (out, err):
    code = run("wait", "--session", "w", "--timeout", "10", "--root", tempfile.gettempdir())
equal("wait without a baseline is refused", code, m.EXIT_USAGE)
check("and the message names the fix", "--after" in err.getvalue(), err.getvalue())

# ---------------------------------------------------------------- 档案
print("\narchives: templates carry the fields a ruling cannot do without")
check("one report naming convention, pinned",
      m.report_path("/r", "001").replace("\\", "/").endswith("/.polity/reports/001.md"),
      m.report_path("/r", "001"))
tmp = tempfile.mkdtemp(prefix="polity-t-")
try:
    with quiet() as (out, err):
        equal("init succeeds", run("--root", tmp, "init"), m.EXIT_OK)
    for rel in ("constitution.md", "ledger.jsonl", "charter/justice.md",
                "charter/pm.md", "charter/worker.md", "templates/task.md",
                "templates/proposal.md", "templates/ruling.md", "templates/report.md"):
        check("init made %s" % rel, os.path.exists(os.path.join(tmp, ".polity", rel)))
    const = open(os.path.join(tmp, ".polity", "constitution.md"), encoding="utf-8").read()
    check("the constitution asks for a source", "出处" in const)
    check("and for a test", "判据" in const)
    with quiet() as (out, err):
        run("--root", tmp, "init")
    equal("init does not overwrite a filled-in constitution",
          open(os.path.join(tmp, ".polity", "constitution.md"), encoding="utf-8").read(), const)
    body = open(os.path.join(tmp, ".polity", "charter", "justice.md"), encoding="utf-8").read()
    check("the justice is told not to grade its own work", "不写实现代码" in body, body[:80])
    pm_body = open(os.path.join(tmp, ".polity", "charter", "pm.md"), encoding="utf-8").read()
    check("the pm is told not to amend the constitution", "不改宪法" in pm_body)

    with quiet() as (out, err):
        equal("new ruling works", run("--root", tmp, "new", "ruling", "007"), m.EXIT_OK)
    path = os.path.join(tmp, ".polity", "rulings", "007.md")
    text = open(path, encoding="utf-8").read()
    for field in ("结论", "依据条款", "证据", "若要修复"):
        check("a ruling template demands %s" % field, field in text)
    check("a ruling demands evidence over self-report", "不接受自述" in text, text)

    with quiet() as (out, err):
        code = run("--root", tmp, "new", "ruling", "007")
    equal("an existing ruling is not overwritten", code, m.EXIT_USAGE)
    check("the file survived", "结论" in open(path, encoding="utf-8").read())

    st = json.load(open(os.path.join(tmp, ".polity", "state.json"), encoding="utf-8"))
    equal("the parallel budget has a default", st.get("max_parallel"), 3)
    ledger = [json.loads(x) for x in
              open(os.path.join(tmp, ".polity", "ledger.jsonl"), encoding="utf-8")
              if x.strip()]
    check("the ledger records the init", any(e.get("kind") == "init" for e in ledger), ledger)
    check("and the new ruling", any(e.get("kind") == "new" and e.get("id") == "007"
                                    for e in ledger), ledger)

    # ------------------------------------------------------------ 派工
    print("\ndispatch: the baseline is read before the send, and the role travels")
    srv = Server(replies=[row(0, "assistant", "DONE 001")])
    with server_installed(srv), quiet() as (out, err):
        code = run("--root", tmp, "dispatch", "--to", "w1", "--task", "001",
                   "--title", "写一个测试", "--body", "只改 a.txt")
    equal("dispatch succeeds", code, m.EXIT_OK)
    names = [c[0] for c in srv.calls]
    check("it prepares the session first", names[0] == "set_session", names)
    base_at = names.index("get_messages")
    send_at = names.index("send_message")
    check("the baseline is read before the send", base_at < send_at, names)
    check("and printed for `wait`", "基线 seq = 0" in out.getvalue(), out.getvalue())
    brief = args_of(srv.calls[send_at][1:])["message"]
    check("the brief is one argument, newlines intact",
          isinstance(brief, str) and "\n" in brief and brief.count("【") >= 1, brief[:60])
    for needle in (os.path.join(tmp, ".polity", "constitution.md"),
                   os.path.join(tmp, ".polity", "charter", "worker.md"),
                   "001", "只做任务单上的事", "不得派生别的 agent",
                   os.path.join(tmp, ".polity", "reports", "001.md"),
                   os.path.join(tmp, ".polity", "templates", "report.md")):
        check("the brief carries %s" % os.path.basename(needle),
              needle in brief, brief)
    task = open(os.path.join(tmp, ".polity", "tasks", "001.md"), encoding="utf-8").read()
    check("the task file got the body", "只改 a.txt" in task, task)
    check("and the completion criteria slot", "完成判据" in task)
    check("the task file names the report path", "reports/001.md" in task.replace("\\", "/"),
          task)
    check("the task file carries the title", "- 标题：写一个测试" in task, task)
    check("and the author", "w1（执行者）" in task, task)
    check("and the template to copy", "templates/report.md" in task.replace("\\", "/"), task)


    srv2 = Server()
    with server_installed(srv2), quiet() as (out, err):
        run("--root", tmp, "dispatch", "--to", "pm", "--task", "002",
            "--title", "统筹", "--role", "pm")
    brief2 = args_of(srv2.calls[-1][1:])["message"]
    # The role line, not just the word: the 铁律 mentions 总理 whoever is being briefed.
    check("a pm dispatch opens as 总理", "【角色】总理" in brief2, brief2[:60])
    check("and a worker dispatch opens as 执行者", "【角色】执行者" in brief, brief[:60])
    check("and points at the pm charter",
          os.path.join(tmp, ".polity", "charter", "pm.md") in brief2)

    with server_installed(Server()), quiet() as (out, err):
        run("--root", tmp, "dispatch", "--to", "w1", "--task", "001",
            "--title", "重复", "--body", "覆盖我")
    check("reusing a task number warns", "不覆盖" in err.getvalue(), err.getvalue())
    check("and leaves the task file alone",
          "覆盖我" not in open(os.path.join(tmp, ".polity", "tasks", "001.md"),
                             encoding="utf-8").read())

    # a dispatch that the client refuses must not be reported as one
    class Refusing(Server):
        def __call__(self, args, timeout=120):
            self.calls.append(list(args))
            if args[0] == "send_message":
                return 0, [{"t": "cmd_result", "id": 1, "ok": False,
                            "message": "boom"}], ""
            return Server.__call__(self, args, timeout)

    with server_installed(Refusing()), quiet() as (out, err):
        code = run("--root", tmp, "dispatch", "--to", "w2", "--task", "003",
                   "--title", "x", "--body", "y")
    equal("a refused dispatch is an error", code, m.EXIT_CLI)
    check("and says why", "boom" in err.getvalue(), err.getvalue())

    # ------------------------------------------------------------ 状态
    print("\nstatus: what the ledger says versus what the server has")
    st = json.load(open(os.path.join(tmp, ".polity", "state.json"), encoding="utf-8"))
    st["pm"] = "pm"
    st["sessions"]["ghost"] = {"role": "worker", "created_at": 5}
    json.dump(st, open(os.path.join(tmp, ".polity", "state.json"), "w",
                       encoding="utf-8"), ensure_ascii=False)
    srv3 = Server()
    srv3.create("pm", created_at=99)
    with server_installed(srv3), quiet() as (out, err):
        code = run("--root", tmp, "status")
    equal("status succeeds", code, m.EXIT_OK)
    check("it lists the live session", "pm" in out.getvalue(), out.getvalue())
    check("it reports a registered session that is gone", "ghost" in err.getvalue(),
          err.getvalue())
    check("it notices a same-name session that was rebuilt",
          "created_at" in err.getvalue(), err.getvalue())

    # ------------------------------------------------------------ 取证
    print("\nverify: the justice reads the repo, not the report")
    with quiet() as (out, err):
        code = run("--root", tmp, "verify")
    equal("verify outside a repo fails, so a clean report means something", code, m.EXIT_CLI)
    repo = os.path.join(tmp, "repo")
    os.makedirs(repo)
    git = shutil.which("git") or "git"
    subprocess.run([git, "init", "-q"], cwd=repo, check=True)
    open(os.path.join(repo, "a.txt"), "w").write("x\n")
    with quiet() as (out, err):
        code = run("--root", repo, "verify")
    equal("verify in a repo succeeds", code, m.EXIT_OK)
    check("and shows the change", "a.txt" in out.getvalue(), out.getvalue())
    with quiet() as (out, err):
        run("--root", repo, "verify")
    check("a clean repo says nothing about being ignored",
          "忽略" not in out.getvalue(), out.getvalue())

    repo2 = os.path.join(tmp, "repo2")
    os.makedirs(os.path.join(repo2, "ignored"))
    subprocess.run([git, "init", "-q"], cwd=repo2, check=True)
    open(os.path.join(repo2, ".gitignore"), "w").write("ignored\n")
    open(os.path.join(repo2, "ignored", "a.txt"), "w").write("x\n")
    with quiet() as (out, err):
        run("--root", os.path.join(repo2, "ignored"), "verify")
    check("a root git ignores is flagged, so an empty diff is not read as clean",
          "忽略" in out.getvalue(), out.getvalue())
    check("and it names mtime/md5 instead", "mtime" in out.getvalue(), out.getvalue())
finally:
    shutil.rmtree(tmp, ignore_errors=True)

# ---------------------------------------------------------------- 真进程
print("\ntranscript: the window the client is asked for")

# `--seq <a:b>` is inclusive at both ends and counts from 1 (SPEC.md:53), so a
# baseline of N is asked for as "N+1:N+limit". Both halves of that arithmetic
# have been wrong in a draft of this file; these two checks are the pins.
srv = Server()
srv.create("t", [row(i, "assistant", "line %d" % i) for i in range(1, 11)])
with server_installed(srv):
    got, tail = m.rows("t", after=0, limit=3)
    equal("a baseline of 0 starts at the first row", [r["seq"] for r in got], [1, 2, 3])
    equal("and the tail still reports the end of the transcript", tail, 10)
    got, _ = m.rows("t", after=7, limit=3)
    equal("a baseline of 7 excludes row 7 itself", [r["seq"] for r in got], [8, 9, 10])
    got, _ = m.rows("t", after=9, limit=5)
    equal("a window past the end is just the rows that exist",
          [r["seq"] for r in got], [10])
    equal("the client is asked in the protocol's own spelling",
          args_of(srv.calls[-1][1:]).get("seq"), "10:14")

print("\nplumbing: a real client process per command")


def shim(directory, state):
    """An executable that runs fake_cli.py, whatever the host calls executables."""
    py = sys.executable
    script = os.path.join(HERE, "fake_cli.py")
    if os.name == "nt":
        path = os.path.join(directory, "fake_cli.bat")
        with open(path, "w", encoding="utf-8", newline="\r\n") as f:
            f.write('@echo off\n"%s" "%s" %%*\n' % (py, script))
    else:
        path = os.path.join(directory, "fake_cli.sh")
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write('#!/bin/sh\nexec "%s" "%s" "$@"\n' % (py, script))
        os.chmod(path, 0o755)
    return path, state


tmp = tempfile.mkdtemp(prefix="polity-p-")
try:
    state = os.path.join(tmp, "state.json")
    cli, _ = shim(tmp, state)
    env = dict(os.environ, GONGGA_CLI=cli, FAKE_CLI_STATE=state)
    real_env = dict(os.environ)
    os.environ.update(GONGGA_CLI=cli, FAKE_CLI_STATE=state)
    try:
        with quiet():
            equal("init through the launcher", run("--root", tmp, "init"), m.EXIT_OK)
        equal("up creates the pm session", run("--root", tmp, "up"), m.EXIT_OK)
        saved = json.load(open(os.path.join(tmp, ".polity", "state.json"), encoding="utf-8"))
        equal("and records it", saved.get("pm"), "pm")
        check("with its created_at", saved["sessions"]["pm"].get("created_at"), saved)
        with quiet() as (out, err):
            code = run("--root", tmp, "status")
        equal("status reads the live server", code, m.EXIT_OK)
        check("and lists the pm session", "pm" in out.getvalue(), out.getvalue())

        os.environ["FAKE_REPLY"] = "DONE 001"
        p = subprocess.run([sys.executable, os.path.join(HERE, "polity.py"),
                            "--root", tmp, "dispatch", "--to", "w1", "--task", "001",
                            "--title", "干活", "--body", "就这一件事"],
                           capture_output=True, text=True, env=os.environ)
        equal("dispatch through the launcher exits 0", p.returncode, m.EXIT_OK)
        check("and prints a baseline", "基线 seq = 0" in p.stdout, p.stdout + p.stderr)
        with open(os.path.join(tmp, ".polity", "ledger.jsonl"), encoding="utf-8") as f:
            events = [json.loads(x) for x in f if x.strip()]
        check("the ledger holds the dispatch with its baseline",
              any(e.get("kind") == "dispatch" and e.get("baseline") == 0
                  for e in events), events)
        # The same turn again, now with rows on disk: the baseline must follow them.
        p = subprocess.run([sys.executable, os.path.join(HERE, "polity.py"),
                            "--root", tmp, "dispatch", "--to", "w1", "--task", "004",
                            "--title", "第二件", "--body", "b"],
                           capture_output=True, text=True, env=os.environ)
        check("a second dispatch takes the current tail as its baseline",
              "基线 seq = 2" in p.stdout, p.stdout + p.stderr)
    finally:
        os.environ.clear()
        os.environ.update(real_env)
finally:
    shutil.rmtree(tmp, ignore_errors=True)

print("\n%s" % ("PASS" if not bad else "%d FAILED" % bad))
sys.exit(1 if bad else 0)
