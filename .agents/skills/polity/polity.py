#!/usr/bin/env python3
"""polity -- 把一个 harness 变成能互相制衡的几个 agent。

三个角色：大法官（与 user 对话的那个会话）写宪法、判合宪性；总理（一个单独
会话）派工、汇总、汇报；执行者只做被派的那一件事。

这个脚本只管三件手写容易出错的事：

1. **把 CLI 的噪声变干净。** `gongga` 每条命令吐多行 JSON，其中 `set_session`
   会把自己收到的参数原样回显，把这种输出直接贴进上下文会自我递归膨胀（实测
   一次 16 KB）。这里的 `read` 只留 (seq, kind, role, text)。
2. **可靠地等到回合结束。** `send_message` 0.25 秒就返回，回合在后台跑；而
   "会话现在是 idle" 也可能只是上一轮留下的状态。所以 `wait` 要一个**派工前
   记录的 seq 基线**，并且要求基线之后真的出现了新的助手正文。
3. **档案的必填字段。** 提案、判决、汇报都按模板生成，缺字段的判决不成立。

宪法和档案都是普通文件，agent 直接读写；脚本不替你思考，只保证时序和格式。
"""

import argparse
import json
import os
import shlex
import subprocess
import sys
import time

EXIT_OK = 0
EXIT_USAGE = 2
EXIT_TIMEOUT = 3
EXIT_CLI = 4
EXIT_NO_REPLY = 5

ROLES = {
    "justice": "大法官",
    "pm": "总理",
    "worker": "执行者",
}

CONSTITUTION = """# 宪法 v0（骨架，条款未生效）

> 每条都必须能判真假：写成"必须/不得/只有当……才"这种句式。
> 没写出处、没写判据的，不算条款。只有大法官能改这份文件。

## 第一条 <一句话说清这条约束>

- 条文：
- 出处：（user 原话，加引号，带日期）
- 判据：看到什么算违反；用什么命令/文件/行为验证。

## 修订记录

- v0 <日期> <会话> 建骨架，条款待填。
"""

TEMPLATES = {
    "task": """# 任务 {id}

- 标题：
- 交给谁：
- 输入（文件/接口/约束，给路径）：
- 交付物（具体到路径）：
- 完成判据（一条能跑的命令，或一个能看的状态）：
- 禁止（不做的事）：
""",
    "proposal": """# 提案 {id}

- 一句话：要做什么。
- 服务哪几条宪法：（逐条引用条款号，并说明怎么满足）
- 会碰到什么：（文件/接口/数据/别人的活；跨了边界就写清楚）
- 可能违反哪条：（自陈风险，宁可多写；写"看不出"也要写）
- 怎么验证做完：（可执行的判据）
- 预算：（几个会话、大概多久）
""",
    "ruling": """# 判决 {id}

- 对象：{id}（提案还是实现）
- 结论：合宪 / 违宪 / 有条件合宪
- 依据条款：（逐条说它满足或违反了第几条，怎么满足/违反）
- 证据：（从总理汇报里点名的证据：路径、行号、命令和它的输出。**不接受自述**；汇报只有结论、没给这些，退回不判）
- 若要修复：（改什么，改到什么程度算合宪）
- 大法官：<会话> 时间：
""",
    "report": """# 汇报 {id}

- 做了什么：（对照任务单逐项）
- 与提案的偏差：（没有就写"无"；有就说明为什么）
- 证据：（命令 + 输出 + 文件:行）
- 没做/没做完的，为什么：
- 需要大法官裁决的问题：
""",
}


# ---------------------------------------------------------------- CLI 通道


def cli_path():
    """Which client to drive. `GONGGA_CLI` is set by the server for every session."""
    return os.environ.get("GONGGA_CLI") or "gongga"


def run_cli(args, timeout=120):
    """Run one client command. Returns (returncode, parsed events, stderr)."""
    try:
        p = subprocess.run(
            [cli_path(), *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            encoding="utf-8",
            errors="replace",
        )
    except FileNotFoundError:
        return EXIT_CLI, [], f"no client at {cli_path()!r}"
    except subprocess.TimeoutExpired:
        return EXIT_CLI, [], f"{args[0]} timed out after {timeout}s"
    return p.returncode, parse_events(p.stdout), p.stderr.strip()


def parse_events(text):
    """The client streams one JSON object per line; anything else is banner noise."""
    out = []
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def cmd_result(events):
    for e in reversed(events):
        if e.get("t") == "cmd_result":
            return e
    return None


def result_of(events, action, err=""):
    """The (ok, message, data) of the command we just ran, or a CLI failure.

    `err` is what the client wrote to stderr -- without it a client that never
    started reads as a client that said nothing, which is the wrong diagnosis.
    """
    r = cmd_result(events)
    if r is None:
        return False, f"{action}: {err or 'the client said nothing'}", {}
    return bool(r.get("ok")), r.get("message", ""), r.get("data") or {}


def sessions_in(events):
    """Session summaries ride along on `sessions` events (and on the greeting)."""
    for e in reversed(events):
        if e.get("t") == "sessions":
            return e.get("sessions") or []
    for e in events:
        if e.get("t") == "hello":
            return e.get("sessions") or []
    return []


def send(session, message):
    """Fire a turn. Returns immediately by design; use `wait` to see it end."""
    rc, evs, err = run_cli(["send_message", "--session", session, "--message", message])
    ok, said, _ = result_of(evs, "send_message", err)
    return (rc == 0 and ok), said or err


def rows(session, after=None, limit=200):
    """Transcript rows, oldest first. `after` is the seq baseline (exclusive).

    `--seq <a:b>` is the protocol's only form (SPEC.md:53): sequence numbers
    count from 1 and both ends are inclusive, so "after N" is "N+1:". That +1
    is the one place an off-by-one can hide, hence the tests pin it.
    """
    first = int(after or 0) + 1
    args = ["get_messages", "--session", session,
            "--seq", f"{first}:{first + int(limit) - 1}"]
    rc, evs, err = run_cli(args)
    ok, said, data = result_of(evs, "get_messages", err)
    if rc != 0 or not ok:
        raise CliError(said or err or "get_messages failed")
    return data.get("messages") or [], data.get("last_seq")


class CliError(Exception):
    pass


def last_seq(session):
    _, tail = rows(session, None, 1)
    return int(tail or 0)


def status_of(session, probe):
    """`set_session` on an existing session is a no-op that reports every status."""
    rc, evs, err = run_cli(["set_session", "--session", probe])
    ok, said, data = result_of(evs, "set_session", err)
    if rc != 0 or not ok:
        raise CliError(said or err or "set_session failed")
    for s in sessions_in(evs):
        if s.get("name") == session:
            return s.get("status"), data
    return None, data


# ---------------------------------------------------------------- 档案


def polity_dir(root):
    return os.path.join(root, ".polity")


def path_in(root, *parts):
    return os.path.join(polity_dir(root), *parts)


def write_new(path, text):
    """Never overwrite: an archive that silently loses a ruling is worse than none."""
    if os.path.exists(path):
        return False
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    return True


def read_text(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return ""


def load_state(root):
    try:
        with open(path_in(root, "state.json"), encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return {"sessions": {}}


def save_state(root, state):
    os.makedirs(polity_dir(root), exist_ok=True)
    with open(path_in(root, "state.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(state, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")


def record(root, **event):
    """One line per decision, so the audit trail does not depend on anyone's memory."""
    event.setdefault("at", time.strftime("%Y-%m-%dT%H:%M:%S"))
    os.makedirs(polity_dir(root), exist_ok=True)
    with open(path_in(root, "ledger.jsonl"), "a", encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")


def report_path(root, task_id):
    """A task number is unique to its author, so the report needs no role suffix.

    One convention, in one place: the brief, the task file and the template all
    point here. Two live agents independently hit the version of this that said
    `<task>.<role>.md` in the brief and `<task>.md` in the task file.
    """
    return path_in(root, "reports", f"{task_id}.md")


def brief(root, role, task_id, title, task_file, constitution):
    """The dispatch message. It is the only carry for a role: the harness has no
    per-session system prompt, so the role must be restated on every dispatch."""
    zh = ROLES[role]
    report = report_path(root, task_id)
    template = path_in(root, "templates", "report.md")
    return f"""【角色】{zh}  任务号 {task_id}：{title}

你是这个政体的{zh}。动手前先读这两份文件：
- 宪法：{constitution}
- 你的职责：{path_in(root, "charter", role + ".md")}

任务单：{task_file}

【铁律】
1. 宪法只有大法官能改。你不得修改 {constitution}。
2. 你不得派生别的 agent（不要用 set_session/send_message 去开新会话）。要人，
   在汇报里向总理申请。
3. 只做任务单上的事。任务单没写的边界，先问，不要自己扩。
4. 结论必须能被独立验证：给路径、行号、命令和它的输出。没验证过的就写"未验证"。
   大法官只读总理的汇报；汇报里只有"已完成、测试通过"这样的结论，会被原样退回。

【交付】复制 {template} 到 {report} 再填；没有模板文件就按这些字段写。
最后一句只回 DONE <任务号>，不要复述全文——原文在文件里，谁要谁读。
"""


# ---------------------------------------------------------------- 命令


def cmd_init(a):
    root = a.root
    made = []
    for sub in ("", "tasks", "proposals", "rulings", "reports", "charter", "templates"):
        d = path_in(root, sub) if sub else polity_dir(root)
        os.makedirs(d, exist_ok=True)
    if write_new(path_in(root, "constitution.md"), CONSTITUTION):
        made.append("constitution.md")
    charters = {
        "justice": """# 大法官

你与 user 对话。你是宪法唯一的作者和唯一的解释者，也是唯一能判合宪性的人。

## 你要做的
1. **先听懂，再立宪。** 把 user 的意图复述回给他："你要的是 X，不要的是 Y，
   边界在 Z"——等他点头，再写进宪法。猜出来的约束不要写成条款。
2. **每条条款都要能判真假。** 句式用"必须/不得/只有当……才"。"要优雅""要注意
   安全"不是条款。每条都带**出处**（user 原话，加引号）和**判据**（看到什么算违反）。
3. **向其他角色解释宪法。** 派工时把宪法文件路径和自己的解释一起给出；别的角色
   对条款有疑问，由你答复，答复写进 `.polity/rulings/` 并加一条修订记录。
4. **判合宪性。** 只读总理的汇报和提案，逐条对照条款。**判决只吃你点名要来的证据**——
   要哪条命令及其输出、哪个路径、哪个 diff，写清楚；然后判这份证据回不回答你的问题。
   总理说"已完成、测试通过"不构成证据，汇报里没有你点名的证据就原样退回，不判。
   **要证据不是取证**：实现、派工、跑测试、取证、读代码都不在你手里。
5. **改宪法要有记录。** 每次修订留一行：版本、日期、谁、为什么、改了什么。

## 你不做的
- 不写实现代码，不派工，不调度，不跑测试，不取证，不读代码下判断。你一旦动手，就失去了判自己的资格。
- 不替总理决定怎么拆任务。你只给目标和边界。

## 判决的三种结果
- **合宪**：可以继续。
- **违宪**：停下，指出违反第几条，要求重做。已经落地的实现要回退。
- **有条件合宪**：可以做，但必须先满足某些条件；条件写清楚，下次判决逐条核。

## 判决模板
用 `polity new ruling <号>` 生成，别手写——少一个字段的判决不算判决。
""",
        "pm": """# 总理

你调度多个执行者把事做完，自己 review、跑测试、取证，然后向大法官汇报。你不解释宪法，也不判自己。

## 循环
1. 读宪法和派工单，确认自己没越界。
2. **有边界不清楚、或者活可能违反某条宪法的，先写提案**（`polity new proposal`），
   交大法官判。判决回来之前不动手。
3. 拆任务：每个任务一个执行者会话，一个任务单（`polity new task`），一条
   `polity dispatch`。任务要小到"一个会话一轮能做完"，并且有可执行的完成判据。
4. 派完用 `polity wait` 等，别干等——一次等一个，等完就收报告文件。
5. 自己 review 回来的东西，自己跑测试，自己取证（`polity verify` 的输出、你跑的命令和
   它的输出、文件:行）——这些大法官不会替你做。写 `polity new report`，然后
   `polity send --tag 汇报` 叫醒大法官。
6. 大法官判违宪，就按判决改，改完重新提请。

## 你不做的
- 不改宪法。
- 不判自己合宪。
- 不在判决回来前动手做"可能违宪"的那部分。
- 不同时开一堆会话：并行度上限写在 `.polity/state.json` 的 `max_parallel`
  （默认 3）。超了就先等。

## 汇报要短
大法官不需要读你的过程。汇报 = 做了什么 / 偏了没有 / 证据 / 要裁决什么。
证据只贴命令和输出（或路径、diff），不写"已完成、测试通过"——大法官会退回。
""",
        "worker": """# 执行者

你只做任务单上的那一件事。

- 读完任务单和它引用的文件再动手。有歧义就在结果里写"未做，因为 X 不清楚"，
  不要猜着做。
- 只碰任务单允许的路径。想改别的地方，写进汇报让总理决定。
- 结论给证据：命令、输出、文件:行。跑过的测试把命令和输出贴出来；没跑的写"未跑"。
- 不派生 agent，不改宪法，不和 user 对话。
- 做完在报告文件里按模板填完，最后只回 `DONE <任务号>`。
""",
    }
    for role, text in charters.items():
        if write_new(path_in(root, "charter", role + ".md"), text):
            made.append(f"charter/{role}.md")
    for kind, text in TEMPLATES.items():
        if write_new(path_in(root, "templates", kind + ".md"), text.format(id="<任务号>")):
            made.append(f"templates/{kind}.md")
    if write_new(path_in(root, "ledger.jsonl"), ""):
        made.append("ledger.jsonl")
    st = load_state(root)
    st.setdefault("max_parallel", 3)
    save_state(root, st)
    record(root, kind="init", made=made)
    print(f"polity at {polity_dir(root)}")
    print("created:", ", ".join(made) if made else "nothing (already there)")
    print("下一步：把 user 的意图写成 constitution.md 的条款，再 `polity up` 开总理会话。")
    return EXIT_OK


def cmd_up(a):
    root = a.root
    os.makedirs(polity_dir(root), exist_ok=True)
    st = load_state(root)
    pm = a.pm or st.get("pm") or "pm"
    rc, evs, err = run_cli(
        ["set_session", "--session", pm, "--cwd", os.path.abspath(a.cwd or root)]
    )
    ok, said, data = result_of(evs, "set_session", err)
    if rc != 0 or not ok:
        print(f"could not create the pm session: {said or err}", file=sys.stderr)
        return EXIT_CLI
    st["pm"] = pm
    st.setdefault("sessions", {})[pm] = {
        "role": "pm",
        "created_at": data.get("created_at"),
        "llm": data.get("llm"),
    }
    save_state(root, st)
    record(root, kind="up", session=pm)
    print(said)
    print(f"\n总理会话 `{pm}` 已就位。派第一个任务：")
    print(f"  polity dispatch --to {pm} --task 001 --title '…' --role pm --body @-")
    return EXIT_OK


def cmd_status(a):
    root = a.root
    st = load_state(root)
    known = st.get("sessions") or {}
    print(f"root: {polity_dir(root)}")
    print(f"宪法: {path_in(root, 'constitution.md')} "
          f"({len(read_text(path_in(root, 'constitution.md')))} 字节)")
    for kind, sub in (("提案", "proposals"), ("判决", "rulings"), ("汇报", "reports")):
        d = path_in(root, sub)
        files = sorted(os.listdir(d)) if os.path.isdir(d) else []
        print(f"{kind}: {len(files)}  {', '.join(files[:6])}")
    if not known:
        print("\n还没有注册任何会话（先 `polity up`）。")
        return EXIT_OK
    # `set_session` on an existing session changes nothing and reports every
    # status; there is no read-only status command in the client.
    probe = st.get("pm") or next(iter(known))
    rc, evs, err = run_cli(["set_session", "--session", probe])
    ok, said, _ = result_of(evs, "set_session", err)
    if rc != 0 or not ok:
        print(f"\n读会话状态失败: {said or err}", file=sys.stderr)
        return EXIT_CLI
    live = {s["name"]: s for s in sessions_in(evs)}
    print(f"\n会话（{len(live)} 个）：")
    for name, s in sorted(live.items()):
        role = (known.get(name) or {}).get("role", "?")
        print(f"  {name:<16} {role:<8} {s.get('status'):<8} msgs={s.get('messages')}")
    for name, meta in known.items():
        got = live.get(name)
        if got is None:
            print(f"  ! {name} 已不存在（记录说 role={meta.get('role')}）", file=sys.stderr)
        elif meta.get("created_at") and got.get("created_at") != meta.get("created_at"):
            print(f"  ! {name} 的 created_at 变了：记录是 {meta['created_at']}，"
                  f"现在是 {got['created_at']}——同名会话被重建过", file=sys.stderr)
    return EXIT_OK


def cmd_send(a):
    msg = a.message if a.message is not None else sys.stdin.read()
    if a.tag:
        msg = f"【{a.tag}】{msg}"
    ok, said = send(a.session, msg)
    print(("ok: " if ok else "FAILED: ") + said)
    return EXIT_OK if ok else EXIT_CLI


def cmd_read(a):
    try:
        got, tail = rows(a.session, a.after, a.limit)
    except CliError as e:
        print(f"read failed: {e}", file=sys.stderr)
        return EXIT_CLI
    for r in got:
        text = " ".join(str(r.get("text", "")).split())
        if len(text) > a.width:
            text = text[: a.width] + "…"
        role = r.get("role") or ""
        print(f"{r.get('seq'):>5} {r.get('kind'):<12} {role:<9} {text}")
    print(f"-- {len(got)} 行，transcript ends at seq {tail}")
    return EXIT_OK


def cmd_dispatch(a):
    root = a.root
    if a.body == "@-":
        body = sys.stdin.read()
    elif a.body and a.body.startswith("@"):
        body = read_text(a.body[1:])
    else:
        body = a.body or ""
    st = load_state(root)
    role = a.role
    target = a.to
    st.setdefault("sessions", {})[target] = {"role": role}
    # Create before dispatching: send_message refuses a session it has never seen.
    rc, evs, err = run_cli(
        ["set_session", "--session", target, "--cwd", os.path.abspath(a.cwd or root)]
    )
    ok, said, data = result_of(evs, "set_session", err)
    if rc != 0 or not ok:
        print(f"could not prepare {target}: {said or err}", file=sys.stderr)
        return EXIT_CLI
    st["sessions"][target]["created_at"] = data.get("created_at")
    save_state(root, st)

    task_file = path_in(root, "tasks", a.task + ".md")
    body_text = TEMPLATES["task"].format(id=a.task)
    # The two facts dispatch already knows: fill them so the task file stands alone.
    body_text = body_text.replace("- 标题：", f"- 标题：{a.title}", 1)
    body_text = body_text.replace("- 交给谁：", f"- 交给谁：{target}（{ROLES[role]}）", 1)
    body_text += "\n## 派工说明\n\n" + body + ("\n" if body else "")
    body_text += (
        "\n## 交付\n\n"
        f"- 报告写到：{report_path(root, a.task)}\n"
        f"- 模板：{path_in(root, 'templates', 'report.md')}"
        "（复制过去再填；不要另起文件名）\n"
        f"- 最后只回 `DONE {a.task}`\n"
    )
    if not write_new(task_file, body_text):
        # The task number already names an order. Reusing it would hand the
        # worker someone else's task, so say it and dispatch what is on disk.
        print(f"{task_file} 已存在，用盘上那一份，不覆盖。", file=sys.stderr)
    constitution = path_in(root, "constitution.md")
    # Baseline BEFORE the send, or a fast turn can finish before we measure.
    try:
        base = last_seq(target)
    except CliError as e:
        print(f"could not read the baseline: {e}", file=sys.stderr)
        return EXIT_CLI
    ok, said = send(target, brief(root, role, a.task, a.title, task_file, constitution))
    if not ok:
        print(f"dispatch failed: {said}", file=sys.stderr)
        return EXIT_CLI
    record(root, kind="dispatch", task=a.task, session=target, role=role, baseline=base)
    print(f"dispatched {a.task} -> {target} ({ROLES[role]})  [{said}]")
    print(f"基线 seq = {base}；等它做完：")
    print(f"  polity wait --session {target} --after {base} --timeout {a.timeout}")
    return EXIT_OK


def wait_loop(session, after, timeout, probe, poll, sleep=time.sleep,
              tail=rows, clock=time.monotonic):
    """Block until the session is idle *and* has spoken after `after`.

    Two-sided on purpose: "idle" alone is also the state left over from the turn
    before, and a new row alone can appear mid-turn. Requiring both is what makes
    the answer the one we dispatched for.

    Returns (code, final_row_or_None, note). Codes: 0 done, 3 timeout,
    5 the turn ended without a closing reply.
    """
    deadline = clock() + timeout
    seen_busy = False
    while True:
        st, _ = status_of(session, probe)
        got, _ = tail(session, after)
        final = [
            r for r in got
            if r.get("kind") == "text" and r.get("role") == "assistant"
        ]
        if st == "running":
            seen_busy = True
        if st == "idle":
            if final:
                return EXIT_OK, final[-1], "done"
            if seen_busy or got:
                return EXIT_NO_REPLY, None, "回合结束了，但没有收尾的助手正文"
        if clock() > deadline:
            if not got and not seen_busy:
                return EXIT_TIMEOUT, None, (
                    "超时，而且基线之后一行都没有——派工可能没落进去"
                )
            return EXIT_TIMEOUT, None, "超时，回合可能还在跑"
        sleep(poll)


def cmd_wait(a):
    if a.after is None:
        print("wait 需要 --after <seq>：派工前的基线。用 `polity dispatch` 派工，"
              "它会打印这个数。", file=sys.stderr)
        return EXIT_USAGE
    st = load_state(a.root)
    probe = st.get("pm") or a.session
    try:
        code, final, note = wait_loop(a.session, a.after, a.timeout, probe, a.poll)
    except CliError as e:
        print(f"wait failed: {e}", file=sys.stderr)
        return EXIT_CLI
    if final is not None:
        print(" ".join(str(final.get("text", "")).split()))
        print(f"-- 收尾于 seq {final.get('seq')}")
    else:
        print(note, file=sys.stderr)
        try:
            got, tail = rows(a.session, a.after, 20)
            for r in got[-5:]:
                print(f"  {r.get('seq')} {r.get('kind')} "
                      f"{' '.join(str(r.get('text','')).split())[:100]}")
            print(f"-- transcript ends at seq {tail}", file=sys.stderr)
        except CliError:
            pass
    return code


def cmd_new(a):
    path = path_in(a.root, a.kind + "s", a.id + ".md")
    if write_new(path, TEMPLATES[a.kind].format(id=a.id)):
        record(a.root, kind="new", of=a.kind, id=a.id)
        print(f"created {path}")
        print("按模板填，字段缺一不可；填完再 `polity send` 或 `polity verify`。")
    else:
        print(f"{path} 已存在，没有覆盖。", file=sys.stderr)
        return EXIT_USAGE
    return EXIT_OK


def cmd_verify(a):
    """Independent evidence for the justice: what actually changed on disk."""
    cmds = [
        ["git", "status", "--short"],
        ["git", "diff", "--stat"],
        ["git", "log", "--oneline", "-3"],
    ]
    if a.range:
        cmds.insert(2, ["git", "diff", "--stat", a.range])
    # A root inside an ignored path makes every git command above look clean, which
    # reads as "nothing changed". Say it before the output is trusted.
    top = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=a.root,
                         capture_output=True, text=True)
    if top.returncode == 0:
        ignored = subprocess.run(["git", "check-ignore", "-q", os.path.abspath(a.root)],
                                 cwd=a.root)
        if ignored.returncode == 0:
            print(f"! {a.root} 被 {top.stdout.strip()} 的 .gitignore 忽略："
                  "下面的 git 输出说明不了这个目录里改了什么。用 mtime/md5 取证。\n")
    rc_all = EXIT_OK
    for c in cmds:
        print(f"$ {' '.join(shlex.quote(x) for x in c)}")
        p = subprocess.run(c, cwd=a.root, capture_output=True, text=True)
        out = (p.stdout or "").rstrip()
        print(out if out else "(empty)")
        print()
        if p.returncode != 0 and c[1] != "log":
            rc_all = EXIT_CLI
    return rc_all


def cmd_event(a):
    record(a.root, kind="note", **({"text": a.text} if a.text else {}))
    print("noted")
    return EXIT_OK


def build_parser():
    p = argparse.ArgumentParser(
        prog="polity",
        description="大法官 / 总理 / 执行者 的多 agent 编排。",
    )
    p.add_argument("--root", default=os.environ.get("POLITY_ROOT") or os.getcwd(),
                   help="项目根（.polity/ 放这里）。默认当前目录。")
    sub = p.add_subparsers(dest="cmd", required=True)

    def sub_root(s):
        """`--root` after the subcommand too: that is where a hand-typed line puts it.

        SUPPRESS so an absent flag leaves the global value alone instead of
        resetting it to the default.
        """
        s.add_argument("--root", default=argparse.SUPPRESS,
                       help="项目根；写在子命令后面也行。")

    s = sub.add_parser("init", help="建 .polity/、宪法骨架和三个角色的章程")
    sub_root(s)
    s.set_defaults(fn=cmd_init)

    s = sub.add_parser("up", help="开总理会话")
    sub_root(s)
    s.add_argument("--pm", help="会话名，默认 pm")
    s.add_argument("--cwd", help="总理会话的工作目录，默认项目根")
    s.set_defaults(fn=cmd_up)

    s = sub.add_parser("status", help="宪法、档案、各会话的状态")
    sub_root(s)
    s.set_defaults(fn=cmd_status)

    s = sub.add_parser("send", help="给一个会话发消息（立即返回）")
    sub_root(s)
    s.add_argument("--session", required=True)
    s.add_argument("--message")
    s.add_argument("--tag", help="加前缀，如 汇报 / 判决")
    s.set_defaults(fn=cmd_send)

    s = sub.add_parser("read", help="紧凑地读 transcript（不带 CLI 的噪声）")
    sub_root(s)
    s.add_argument("--session", required=True)
    s.add_argument("--after", type=int, help="只看 seq 大于它的行")
    s.add_argument("--limit", type=int, default=200)
    s.add_argument("--width", type=int, default=120)
    s.set_defaults(fn=cmd_read)

    s = sub.add_parser("dispatch", help="写任务单并派给一个会话")
    sub_root(s)
    s.add_argument("--to", required=True)
    s.add_argument("--task", required=True, help="任务号，如 001")
    s.add_argument("--title", required=True)
    s.add_argument("--role", default="worker", choices=sorted(ROLES))
    s.add_argument("--body", help="派工说明；@- 读 stdin，@path 读文件")
    s.add_argument("--cwd")
    s.add_argument("--timeout", type=int, default=300, help="只用于打印建议命令")
    s.set_defaults(fn=cmd_dispatch)

    s = sub.add_parser("wait", help="等到回合结束，打印收尾那一句")
    sub_root(s)
    s.add_argument("--session", required=True)
    s.add_argument("--after", type=int, required=False)
    s.add_argument("--timeout", type=int, default=300)
    s.add_argument("--poll", type=float, default=1.5)
    s.set_defaults(fn=cmd_wait)

    s = sub.add_parser("new", help="按模板建档案（task/proposal/ruling/report）")
    sub_root(s)
    s.add_argument("kind", choices=sorted(TEMPLATES))
    s.add_argument("id")
    s.set_defaults(fn=cmd_new)

    s = sub.add_parser("verify", help="独立取证：git 上到底改了什么")
    sub_root(s)
    s.add_argument("--range", help="git 范围，如 HEAD~3..HEAD")
    s.set_defaults(fn=cmd_verify)

    s = sub.add_parser("event", help="往 ledger 记一条")
    sub_root(s)
    s.add_argument("--text")
    s.set_defaults(fn=cmd_event)
    return p


def main(argv=None):
    a = build_parser().parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
