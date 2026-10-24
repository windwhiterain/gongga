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
    check("the justice charter never runs wait",
          "polity wait --session" not in body and "$P wait" not in body, body)
    check("it says wait belongs to the pm", "`polity wait` 是总理" in body, body)
    check("and that the justice is woken instead", "结束回合" in body and "唤醒" in body, body)
    check("the justice is told not to grade its own work", "不写实现代码" in body, body[:80])
    check("the justice is told demanding is not gathering", "要证据不是取证" in body, body)
    check("and that test runs and evidence are not its job",
          "不跑测试" in body and "不取证" in body, body)
    pm_body = open(os.path.join(tmp, ".polity", "charter", "pm.md"), encoding="utf-8").read()
    check("the pm is told not to amend the constitution", "不改宪法" in pm_body)
    check("and to review, test and gather the evidence itself",
          "跑测试" in pm_body and "取证" in pm_body, pm_body)

    with quiet() as (out, err):
        equal("new ruling works", run("--root", tmp, "new", "ruling", "007"), m.EXIT_OK)
    path = os.path.join(tmp, ".polity", "rulings", "007.md")
    text = open(path, encoding="utf-8").read()
    for field in ("结论", "依据条款", "证据", "若要修复"):
        check("a ruling template demands %s" % field, field in text)
    check("a ruling demands evidence over self-report", "不接受自述" in text, text)
    check("and sends back a report that only asserts", "退回不判" in text, text)

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
    # 铁律 2 forks: forbidding the pm to open sessions makes SPEC.md:79-81 impossible.
    check("a pm brief does not forbid dispatching", "不得派生" not in brief2, brief2)
    check("it tells the pm to dispatch and report the parallelism",
          "你要派生执行者" in brief2 and "并行度" in brief2, brief2)
    check("a worker brief still forbids it", "不得派生别的 agent" in brief, brief)
    check("and says nothing about parallelism", "并行度" not in brief, brief)

    print("\nsend: the help says what it does -- wake the justice, --session for others")
    helped = io.StringIO()
    with contextlib.redirect_stdout(helped):
        try:
            run("--help")
        except SystemExit:
            pass
    listed = [l for l in helped.getvalue().splitlines() if l.strip().startswith("send")]
    check("the one-line summary names the judge as the addressee",
          listed and "叫醒法官" in listed[0], listed)
    check("and says --session goes elsewhere",
          listed and "指定别的会话" in listed[0], listed)
    helped = io.StringIO()
    with contextlib.redirect_stdout(helped):
        try:
            run("send", "--help")
        except SystemExit:
            pass
    helptext = helped.getvalue()
    check("--session's own help says omitting it wakes the judge",
          "省略就是叫醒法官" in helptext, helptext)
    check("and that giving it addresses that session", "给了就发给那个会话" in helptext, helptext)
    # Ruling 006 §4: "wake" and "returned" are two facts and must not share a line.
    # The failure this guards is reading "sent" as "done".
    body = helptext.splitlines()
    wake = [i for i, l in enumerate(body) if "叫醒法官" in l]
    back = [i for i, l in enumerate(body) if "立即返回" in l]
    check("send --help says both facts", wake and back, body[:8])
    if wake and back:
        check("and they are on different lines", wake[0] != back[0], (wake, back, body[:8]))
        check("with the return meaning spelled out", "不代表对方做完" in body[back[0]], body[back[0]])
    check("the one line the parent list holds says the same",
          listed and "不代表对方做完了" in listed[0], listed)

    # ---- the client gets a home of its own, so no court command writes the user's
    # per-session client state (SPEC.md:96-97). The check is on what the client was
    # actually run with, not on our intention.
    print("\nclient home: every client call runs with --home on polity's own directory")
    src = os.path.join(tmp, "user-home")
    os.makedirs(src)
    open(os.path.join(src, "config.toml"), "w", encoding="utf-8").write("[server]\nkey = \"k\"\n")
    home = m.prepare_client_home(tmp, source=src)
    equal("the private home is inside .polity",
          os.path.abspath(home), os.path.abspath(os.path.join(tmp, ".polity", "client-home")))
    check("it carries the connection key", os.path.exists(os.path.join(home, "config.toml")),
          os.listdir(home))
    argv = m.client_argv(["set_session", "--session", "x"])
    check("the client is told to use it", argv[1:3] == ["--home", os.path.abspath(home)], argv)
    check("and the command still comes last", argv[-3:] == ["set_session", "--session", "x"], argv)
    # A command that never drives the client must not litter the root with a home.
    other = os.path.join(tmp, "no-client")
    os.makedirs(other)
    m.CLIENT_HOME, keep = None, m.CLIENT_HOME
    try:
        with quiet():
            run("--root", other, "new", "report", "009")
    finally:
        m.CLIENT_HOME = keep
    check("a command with no client call creates no client home",
          not os.path.exists(os.path.join(other, ".polity", "client-home")))

    # ---- the key in that home must never reach an output ----------------------
    # SPEC.md:75-78 makes the court an instrument: a key echoed into a terminal,
    # a log or the ledger is handed to whoever reads that file next.
    print("\nkey: the client home carries it, and no command ever prints it")
    secret = "sekrit-key-9d41c0ffee"
    # Write it where `main` will point the client: it resets CLIENT_HOME per run and
    # then reuses whatever config the home already has.
    home = os.path.join(tmp, ".polity", "client-home")
    os.makedirs(home, exist_ok=True)
    open(os.path.join(home, "config.toml"), "w", encoding="utf-8").write(
        "[server]\nbind = \"127.0.0.1:1\"\nkey = \"%s\"\n" % secret)
    equal("the key is read from the home", m.client_key(home), secret)
    equal("and redacted, not printed", m.redact("boom " + secret + " tail", secret),
          "boom <key> tail")

    class Leaky(Server):
        """A client that fails loudly and quotes the key while doing it."""

        def __call__(self, args, timeout=120):
            self.calls.append(list(args))
            if args[0] == "set_session":
                return 0, [], "connection refused; I tried key %s at that address" % secret
            return Server.__call__(self, args, timeout)

    with server_installed(Leaky()), quiet() as (out, err):
        code = run("--root", tmp, "status")
    equal("a client failure is still a failure", code, m.EXIT_CLI)
    both = out.getvalue() + err.getvalue()
    check("its complaint is passed through", "connection refused" in both, both)
    check("but the key is not in it", secret not in both, both)
    check("the redaction is visible", "<key>" in both, both)
    # The stdout side: the sessions event the client sends back.
    class LeakyStdout(Server):
        def __call__(self, args, timeout=120):
            self.calls.append(list(args))
            rc, evs, err = Server.__call__(self, args, timeout)
            for e in evs:
                if e.get("t") == "cmd_result":
                    e["message"] = "ok, key=%s" % secret
            return rc, evs, err

    with server_installed(LeakyStdout()), quiet() as (out, err):
        run("--root", tmp, "up", "--justice", "judge-x")
        run("--root", tmp, "send", "--message", "m")
    both = out.getvalue() + err.getvalue()
    check("what the client says on stdout is scrubbed too", secret not in both, both)
    ledger = open(os.path.join(tmp, ".polity", "ledger.jsonl"), encoding="utf-8").read()
    check("and the ledger never holds it", secret not in ledger, ledger[-300:])

    # `verify` prints `git` output, which is repo content: scrub it there too. Git is
    # replaced only for the duration of the call, so the check is about polity, not git.
    class FakeGit:
        returncode = 0
        stdout = "+ connection_key = %s\n" % secret
        stderr = ""

    real_run = m.subprocess.run
    m.subprocess.run = lambda *a, **k: FakeGit()
    try:
        with quiet() as (out, err):
            code = run("--root", tmp, "verify")
    finally:
        m.subprocess.run = real_run
    both = out.getvalue() + err.getvalue()
    check("verify scrubs what git prints", secret not in both and "<key>" in both, both)

    # init must leave a .gitignore behind: that home is inside the root, and the
    # root may be a repo.
    ign = os.path.join(tmp, ".polity", ".gitignore")
    check("init lands .polity/.gitignore", os.path.exists(ign), os.listdir(os.path.join(tmp, ".polity")))
    if os.path.exists(ign):
        text = open(ign, encoding="utf-8").read()
        check("it ignores client-home/", "client-home/" in text, text)
        with quiet():
            run("--root", tmp, "init")
        equal("and init does not rewrite it", open(ign, encoding="utf-8").read(), text)
    repo3 = os.path.join(tmp, "repo3")
    os.makedirs(os.path.join(repo3, ".polity", "client-home"))
    open(os.path.join(repo3, ".polity", "client-home", "config.toml"), "w").write("key = \"x\"\n")
    with quiet():
        run("--root", repo3, "init")
    subprocess.run([shutil.which("git") or "git", "init", "-q"], cwd=repo3, check=True)
    ignored = subprocess.run([shutil.which("git") or "git", "check-ignore", "-q",
                              os.path.join(repo3, ".polity", "client-home", "config.toml")],
                             cwd=repo3)
    equal("so git refuses that copy", ignored.returncode, 0)

    print("\nwake: the court records the justice, and send finds it without a name")
    srv4 = Server()
    with server_installed(srv4), quiet() as (out, err):
        code = run("--root", tmp, "up", "--justice", "judge-x")
    equal("up with --justice succeeds", code, m.EXIT_OK)
    saved = json.load(open(os.path.join(tmp, ".polity", "state.json"), encoding="utf-8"))
    equal("the court records which session the justice is", saved.get("justice"), "judge-x")
    srv4.create("judge-x")
    with server_installed(srv4), quiet() as (out, err):
        code = run("--root", tmp, "send", "--tag", "汇报", "--message", "报告好了")
    equal("send without --session succeeds", code, m.EXIT_OK)
    sent_to = [args_of(c[1:]).get("session") for c in srv4.calls if c[0] == "send_message"]
    equal("and it goes to the recorded justice", sent_to, ["judge-x"])
    srv4.create("someone-else")
    with server_installed(srv4), quiet() as (out, err):
        code = run("--root", tmp, "send", "--session", "someone-else", "--message", "m")
    equal("an explicit --session still wins", code, m.EXIT_OK)
    equal("and goes where it says",
          [args_of(c[1:]).get("session") for c in srv4.calls if c[0] == "send_message"][-1],
          "someone-else")

    print("\nwait: waiting on the justice warns -- it is not polled (SPEC.md:89-92)")
    srvw = Server()
    srvw.create("judge-x", [row(1, "assistant", "DONE 001")])
    with server_installed(srvw), quiet() as (out, err):
        code = run("--root", tmp, "wait", "--session", "judge-x", "--after", "0",
                   "--timeout", "2")
    check("the wait on the justice is not refused", code in (m.EXIT_OK, m.EXIT_TIMEOUT), code)
    check("it says the justice is not polled", err.getvalue().count("法官") >= 1,
          err.getvalue())
    check("and cites the clause", "89-92" in err.getvalue(), err.getvalue())
    # A worker gets no such warning: only the justice is the special case.
    srvw.create("w-doc", [row(1, "assistant", "DONE 002")])
    with server_installed(srvw), quiet() as (out, err):
        run("--root", tmp, "wait", "--session", "w-doc", "--after", "0", "--timeout", "2")
    check("a worker wait says nothing about the justice", "不被轮询" not in err.getvalue(),
          err.getvalue())

    # A court that never recorded its justice must refuse, not guess.
    stj = json.load(open(os.path.join(tmp, ".polity", "state.json"), encoding="utf-8"))
    stj.pop("justice", None)
    json.dump(stj, open(os.path.join(tmp, ".polity", "state.json"), "w", encoding="utf-8"),
              ensure_ascii=False)
    srv5 = Server()
    with server_installed(srv5), quiet() as (out, err):
        code = run("--root", tmp, "send", "--message", "m")
    equal("no recorded justice and no --session is a usage error", code, m.EXIT_USAGE)
    check("and it says how to record one", "up --justice" in err.getvalue(), err.getvalue())
    check("and nothing was sent", not [c for c in srv5.calls if c[0] == "send_message"],
          srv5.calls)
    stj["justice"] = "judge-x"
    json.dump(stj, open(os.path.join(tmp, ".polity", "state.json"), "w", encoding="utf-8"),
              ensure_ascii=False)

    # The brief the pm gets must carry a wake line that runs as written (SPEC.md:93-95).
    print("\nwake: the pm's brief carries a command that runs as written")
    srv6 = Server()
    with server_installed(srv6), quiet() as (out, err):
        run("--root", tmp, "dispatch", "--to", "pm2", "--task", "010",
            "--title", "统筹", "--role", "pm")
    brief3 = args_of(srv6.calls[-1][1:])["message"]
    check("the pm brief names the justice", "judge-x" in brief3, brief3)
    check("and carries the launcher, not a bare `polity`",
          m.launcher() in brief3, m.launcher())
    check("and the wake command omits --session",
          "send --root" in brief3 and "--session" not in brief3, brief3)
    check("and tells the pm not to wait on the justice",
          "不要 `polity wait` 我" in brief3, brief3)
    check("it says the wake is the handover", "做完就叫醒我" in brief3, brief3)
    check("and why waiting is pointless", "结束回合" in brief3, brief3)
    # The other branch: no justice recorded, so there is no command to hand over --
    # but "do not wait the justice" must still be said (it does not need a name).
    stj2 = json.load(open(os.path.join(tmp, ".polity", "state.json"), encoding="utf-8"))
    recorded = stj2.pop("justice", None)
    json.dump(stj2, open(os.path.join(tmp, ".polity", "state.json"), "w", encoding="utf-8"),
              ensure_ascii=False)
    try:
        srv7 = Server()
        with server_installed(srv7), quiet():
            run("--root", tmp, "dispatch", "--to", "pm3", "--task", "011",
                "--title", "统筹", "--role", "pm")
        brief4 = args_of(srv7.calls[-1][1:])["message"]
    finally:
        stj2["justice"] = recorded
        json.dump(stj2, open(os.path.join(tmp, ".polity", "state.json"), "w", encoding="utf-8"),
                  ensure_ascii=False)
    check("with no justice recorded it still says do not wait", "不要 `polity wait` 法官" in brief4,
          brief4)
    check("and it does not invent a name", "会话 `" not in brief4.split("【交活】")[1].split("】")[0],
          brief4)
    check("a worker brief has no wake block", "交活" not in brief, brief)

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

    # A charter on disk is a copy taken at init time, and write_new never
    # refreshes it: an old court would read the old rule forever. Warn, never block.
    print("\nstatus: a landed charter older than the built-in text is called out")
    with server_installed(Server()), quiet() as (out, err):
        code = run("--root", tmp, "status")
    equal("a court whose charters match is not called drift", code, m.EXIT_OK)
    check("and nothing is said about charters", "章程/模板" not in err.getvalue(),
          err.getvalue())
    charter = os.path.join(tmp, ".polity", "charter", "justice.md")
    want = open(charter, encoding="utf-8").read()
    open(charter, "w", encoding="utf-8").write(
        want.replace("只读总理的汇报和提案", "读提案和实现"))
    with server_installed(Server()), quiet() as (out, err):
        code = run("--root", tmp, "status")
    equal("a drifted charter does not change the exit code", code, m.EXIT_OK)
    check("it names the file", "charter/justice.md" in err.getvalue(), err.getvalue())
    check("and the line it diverges at", "行起不同" in err.getvalue(), err.getvalue())
    check("and both md5s", err.getvalue().count("md5") >= 2, err.getvalue())
    os.remove(os.path.join(tmp, ".polity", "templates", "proposal.md"))
    with server_installed(Server()), quiet() as (out, err):
        code = run("--root", tmp, "status")
    equal("a missing template is not fatal either", code, m.EXIT_OK)
    check("and it is called missing", "templates/proposal.md" in err.getvalue()
          and "缺" in err.getvalue(), err.getvalue())
    open(charter, "w", encoding="utf-8").write(want)

    # ------------------------------------------------------------ 取证
    print("\nverify: the pm gathers it, the justice reads it")
    helped = io.StringIO()
    with contextlib.redirect_stdout(helped):
        try:
            run("--help")
        except SystemExit:
            pass
    # "独立" read as "this belongs to the justice" (SPEC.md:77-78, 85-88).
    # The subcommand list is where the one-line `help=` shows up.
    listed = [l for l in helped.getvalue().splitlines() if l.strip().startswith("verify")]
    check("verify's help does not claim the evidence is the justice's",
          listed and "独立取证" not in "\n".join(listed) and "取证" in "\n".join(listed),
          listed)
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
    # A stand-in for the user's home: if polity ever pointed the client at it, the
    # fake would write its sticky there and these checks would see it.
    user_home = os.path.join(tmp, "user-home")
    os.makedirs(user_home)
    open(os.path.join(user_home, "config.toml"), "w", encoding="utf-8").write(
        "[server]\nkey = \"from-user-home\"\n")
    argv_log = os.path.join(tmp, "argv.jsonl")
    env = dict(os.environ, GONGGA_CLI=cli, FAKE_CLI_STATE=state,
               GONGGA_HOME=user_home, FAKE_CLI_ARGV=argv_log)
    real_env = dict(os.environ)
    os.environ.update(GONGGA_CLI=cli, FAKE_CLI_STATE=state,
                      GONGGA_HOME=user_home, FAKE_CLI_ARGV=argv_log)
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

        # -- the zero-write claim, checked on the child processes themselves --
        calls = [json.loads(x) for x in open(argv_log, encoding="utf-8") if x.strip()]
        check("the client calls so far are on record", len(calls) >= 2, calls)
        homes = [c["global"].get("home") for c in calls]
        want_home = os.path.abspath(os.path.join(tmp, ".polity", "client-home"))
        check("each one ran with --home on polity's own directory",
              all(h and os.path.abspath(h) == want_home for h in homes), homes)
        check("none of them ran on the user's home",
              all(os.path.abspath(h) != os.path.abspath(user_home) for h in homes if h), homes)
        check("the private home carries the connection key",
              os.path.exists(os.path.join(want_home, "config.toml")), os.listdir(want_home))
        check("and the user's home gained no cli.toml",
              not os.path.exists(os.path.join(user_home, "cli.toml")),
              os.listdir(user_home))

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
        # Every command that drove the client -- up, status, two dispatches, each
        # with a set_session + get_messages + send_message inside it -- must have
        # run on polity's own home. This is the zero-write proof, from the children.
        calls = [json.loads(x) for x in open(argv_log, encoding="utf-8") if x.strip()]
        check("dispatch drove the client too", len(calls) >= 6, len(calls))
        check("and every call kept --home on the private directory",
              all(h and os.path.abspath(h) == want_home
                  for h in (c["global"].get("home") for c in calls)),
              [c["global"].get("home") for c in calls])
        check("still no cli.toml in the user's home",
              not os.path.exists(os.path.join(user_home, "cli.toml")),
              os.listdir(user_home))
    finally:
        os.environ.clear()
        os.environ.update(real_env)
finally:
    shutil.rmtree(tmp, ignore_errors=True)

print("\n%s" % ("PASS" if not bad else "%d FAILED" % bad))
sys.exit(1 if bad else 0)
