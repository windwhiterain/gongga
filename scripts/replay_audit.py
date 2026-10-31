"""R1-R3 / R5: nothing may be sent that the database does not hold.

Drives a real server, a real turn with a tool call, a prose edit between turns and
a fold mid-conversation, then reads the request bodies the provider was given and
the rows that were written, and checks them against each other.
"""

import io
import json
import os
import sqlite3
import re
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRATCH = tempfile.mkdtemp(prefix="gongga-replay-")
HOME = os.path.join(SCRATCH, "home")
DUMP = os.path.join(SCRATCH, "dump.jsonl")
LOG = os.path.join(SCRATCH, "server.log")
PORT = "18972"
FAKE_PORT = "8785"
CLI = os.path.join(ROOT, "target", "debug", "gongga-cli.exe")

failures = []


def check(name, ok, detail=""):
    print(("  ok   " if ok else "  FAIL ") + name + (f"  {detail}" if detail else ""))
    if not ok:
        failures.append(name)


def spawn(cmd, env=None, out=None):
    handle = io.open(LOG, "a", encoding="utf-8") if out == "log" else (out or subprocess.DEVNULL)
    child = subprocess.Popen(cmd, env=env, stdout=handle, stderr=subprocess.STDOUT)
    if out == "log":
        handle.close()  # the child holds the fd; an open writer here hides its flushes
    return child


def taskkill(pid):
    subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)], capture_output=True)


def main():
    os.makedirs(HOME, exist_ok=True)

    # A roster written here, naming only the fake. The machine's own roster is never
    # read: an audit that reaches a real provider spends somebody's quota, and the
    # first line below is the whole defence -- there is no URL in it but 127.0.0.1.
    roster_path = os.path.join(SCRATCH, "roster.toml")
    io.open(roster_path, "w", encoding="utf-8", newline="").write(
        "[[provider]]\nid = \"p\"\nkey = \"dummy\"\n"
        "[[provider.endpoint]]\napi = \"openai-responses\"\n"
        f"base_url = \"http://127.0.0.1:{FAKE_PORT}/v1\"\n"
        "[[provider.model]]\nid = \"m1\"\n")

    env = dict(os.environ)
    env["GONGGA_API_KEY"] = "dummy"
    env["GONGGA_LLM_CATALOG"] = roster_path
    env["GONGGA_PROVIDERS_CATALOG"] = roster_path
    env["RUST_LOG"] = "info"
    fake_env = dict(env, FAKE_LLM_DUMP=DUMP, FAKE_LLM_TOOL="bash", FAKE_LLM_IMAGE_GEN="1")

    fake = spawn([sys.executable, os.path.join(ROOT, "scripts", "fake_llm.py"), FAKE_PORT],
                 env=fake_env)
    time.sleep(2)
    server = spawn([CLI, "--home", HOME, "serve", "--port", PORT],
                   env=dict(env, GONGGA_HOME=HOME), out="log")
    try:
        time.sleep(16)
        key = io.open(os.path.join(HOME, "server.key"), encoding="utf-8").read().strip()
        base = [CLI, "--server", f"127.0.0.1:{PORT}", "--home", HOME, "--key", key]

        def command(*args):
            subprocess.run(base + list(args), capture_output=True, text=True)

        command("set_session", "--session", "audit")
        command("send_message", "--session", "audit", "--message", "第一回合，会调一个工具")
        time.sleep(8)

        # A prose edit between turns: it must land as a new row, not rewrite the old.
        cfg = os.path.join(HOME, "config.toml")
        raw = io.open(cfg, "rb").read()
        anchor = b"Do not ask for confirmation; the user has already authorised bash execution."
        assert raw.count(anchor) == 1
        io.open(cfg + ".tmp", "wb").write(raw.replace(anchor, anchor + b" Edited between turns."))
        os.replace(cfg + ".tmp", cfg)
        command("send_message", "--session", "audit", "--message", "第二回合，改过的散文")
        time.sleep(8)

        # A handover: new content entering the store mid-conversation.
        db = sqlite3.connect(os.path.join(HOME, "gongga.db"), timeout=5)
        db.row_factory = sqlite3.Row
        sid = db.execute("select id from sessions where name='audit'").fetchone()["id"]
        db.execute("update sessions set handover='一份交接笔记' where id=?", (sid,))
        db.commit()
        db.close()
        command("send_message", "--session", "audit", "--message", "第三回合，笔记之后")
        time.sleep(8)

        # Deleting a session has to take the payload with it -- and it has to be
        # asked while the server is the one answering, because that is who owns
        # the reachability question.
        # Read the transcript first: the delete below is what is being tested, and
        # the checks that follow ask what the rows said before it.
        db = sqlite3.connect(os.path.join(HOME, "gongga.db"), timeout=5)
        db.row_factory = sqlite3.Row
        sid = db.execute("select id from sessions where name='audit'").fetchone()["id"]
        blobs_before = sorted(os.listdir(os.path.join(HOME, "blobs")))
        rows = [(r["seq"], r["kind"], r["display"]) for r in db.execute(
            "select seq, kind, display from messages where session_id=? order by seq", (sid,))]
        db.close()
        deleted = subprocess.run(base + ["del_session", "--session", "audit"],
                                 capture_output=True, text=True)
        print("   del_session:", (deleted.stdout or deleted.stderr).strip()[:110])
        time.sleep(3)
    finally:
        taskkill(server.pid)
        taskkill(fake.pid)
        time.sleep(2)

    requests = [json.loads(l) for l in io.open(DUMP, encoding="utf-8") if l.strip()]
    rows = [{"seq": s, "kind": k, "content": c} for s, k, c in rows]
    prompts = [json.loads(r["content"]) for r in rows if r["kind"] == "system"]

    print(f"== {len(requests)} requests, {len(rows)} rows, {len(prompts)} stored prompts")

    sent_texts = {r.get("instructions", "") for r in requests}
    stored_texts = {p["instructions"] for p in prompts}
    check("every instructions text the provider saw is a stored row",
          sent_texts <= stored_texts, f"{len(sent_texts)} sent / {len(stored_texts)} stored")
    check("a prose edit added a row instead of rewriting one",
          len(stored_texts) == len(sent_texts) and len(prompts) >= 2,
          f"{len(prompts)} rows")

    notes = {p.get("handover") for p in prompts if p.get("handover")}

    def flat(item):
        c = item.get("content")
        if isinstance(c, list):
            return " ".join(x.get("text", "") for x in c)
        return c if isinstance(c, str) else json.dumps(item, sort_keys=True)[:60]

    def shape(kind, content):
        v = json.loads(content)
        if kind in ("tool_call", "tool_result"):
            return (kind, v.get("call_id"))
        if kind == "text":
            return ("assistant" if v.get("role") == "assistant" else "user", v.get("text"))
        if kind == "builtin_call":
            # A provider-run item is known by its own id, not by a call it made.
            return (kind, (v.get("item") or {}).get("id"))
        return (kind, None)

    stored_shapes = {shape(r["kind"], r["content"]) for r in rows
                     if r["kind"] not in ("system", "notice")}

    unexplained = []
    items = 0
    for i, req in enumerate(requests):
        for item in req.get("input", []):
            items += 1
            t = item.get("type")
            if t == "function_call":
                s = ("tool_call", item["call_id"])
            elif t == "function_call_output":
                s = ("tool_result", item["call_id"])
            elif isinstance(t, str) and t.endswith("_call"):
                s = ("builtin_call", item.get("id"))
            elif item.get("role") == "assistant":
                s = ("assistant", flat(item))
            else:
                s = ("user", flat(item))
            if s in stored_shapes or (s[1] in notes):
                continue
            unexplained.append((i, s))
    check(f"every one of the {items} sent items is in the database",
          not unexplained, f"{len(unexplained)} unexplained")
    for u in unexplained[:3]:
        print("      ", u)

    calls = len([r for r in rows if r["kind"] == "tool_call"])
    results = len([r for r in rows if r["kind"] == "tool_result"])
    check("every call has a stored result row", calls == results, f"{calls} calls, {results} results")

    # tracing colours its fields, so `cached=` in the file is `cached\e[0m\e[2m=\e[0m0`:
    # strip the escapes before matching, or a guard passes because it matched nothing.
    blobs_dir = os.path.join(HOME, "blobs")
    check("the payload went beside the home, not into the row",
          any(f.endswith(".json") for f in blobs_before)
          and not any(f.endswith(".part") for f in blobs_before),
          f"{blobs_before}")
    inline = [r for r in rows if r["kind"] == "builtin_call" and "result" in json.loads(r["content"])["item"]]
    check("no base64 rides in a transcript row", not inline, f"{len(inline)} row(s)")
    named = [json.loads(r["content"])["item"].get("result_blob") for r in rows if r["kind"] == "builtin_call"]
    check("and the row names the file that holds it", all(named), f"{named}")
    # A later request has to carry the item with its payload back: that is the whole
    # reason the file exists.
    sent = [r for r in requests if any(i.get("type") == "image_generation_call" for i in r["input"])]
    carried = [i for r in sent for i in r["input"] if i.get("type") == "image_generation_call"]
    check("a later request carries the item with its payload reattached",
          bool(carried) and all(i.get("result") and "result_blob" not in i for i in carried),
          f"{len(sent)} request(s), {len(carried)} item(s)")

    log = re.sub(r"\[[0-9;]*m", "", io.open(LOG, encoding="utf-8", errors="replace").read())
    check("the boundary where the prose changed was logged",
          "the prompt in force changed" in log)
    sample = next((l for l in log.splitlines() if "provider usage" in l), "(none)")
    check("the usage lines carry a cached count", "cached=" in sample,
          f"{log.count('provider usage')} line(s); sample: {sample[-96:]!r}")
    urls = set()
    for line in log.splitlines():
        at = line.find("url=")
        if "model request: sending" in line and at >= 0:
            urls.add(line[at + 4:].split()[0])
    check("every request went to the fake, never a real provider",
          all(u.startswith(f"http://127.0.0.1:{FAKE_PORT}") for u in urls),
          f"{len(urls)} target(s): {sorted(urls)}")

    left = sorted(os.listdir(blobs_dir)) if os.path.isdir(blobs_dir) else []
    check("deleting the session took the payload with it", not left, f"{left}")

    print("FAILURES:", failures or "none")
    if os.environ.get("GONGGA_REPLAY_KEEP"):
        print("scratch:", SCRATCH)
    else:
        subprocess.run(["cmd", "/c", "rmdir", "/s", "/q", SCRATCH], capture_output=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
