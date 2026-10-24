#!/usr/bin/env python3
"""A stand-in for the `gongga` client: just enough of it to drive polity.py.

State lives in the JSON file named by `FAKE_CLI_STATE`, because polity.py starts a
new client process per command -- the same thing the real client does. Sessions
persist across invocations only if they persist on disk.

Two behaviours it deliberately copies, because polity.py is built around them:

  * `send_message` returns at once and the turn happens "later". `FAKE_REPLY` says
    what the assistant says, and `FAKE_STATUS` says what the session reports next
    (default `idle`, i.e. an instant turn).
  * every command prints a `sessions` event *and* an echo line that is not JSON, so
    a parser that trusts the first line gets caught.
"""
import json
import os
import sys

STATE = os.environ.get("FAKE_CLI_STATE") or "fake-state.json"


def load():
    try:
        with open(STATE, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return {"sessions": {}, "next_created": 1792837000000}


def save(state):
    with open(STATE, "w", encoding="utf-8", newline="\n") as f:
        json.dump(state, f, ensure_ascii=False, indent=2, sort_keys=True)


def emit(obj):
    print(json.dumps(obj, ensure_ascii=False), flush=True)


def summary(state):
    return [
        {
            "name": name,
            "status": s.get("status", "idle"),
            "created_at": s.get("created_at"),
            "updated_at": s.get("created_at"),
            "messages": len(s.get("rows", [])),
            "cwd": s.get("cwd", ""),
        }
        for name, s in sorted(state["sessions"].items())
    ]


def args_of(argv):
    out = {}
    i = 0
    while i < len(argv):
        if argv[i].startswith("--"):
            key = argv[i][2:]
            val = argv[i + 1] if i + 1 < len(argv) else ""
            out[key] = val
            i += 2
        else:
            i += 1
    return out


def parse_seq(raw, last):
    """`<a>:<b>`, both ends optional and inclusive; negative counts back from
    the newest row. Mirrors the server's parse_seq_slice, including its refusal
    of 0 -- a fake that is more permissive than the real thing hides bugs."""
    if raw is None:
        raise ValueError("get_messages requires --seq")
    start_s, _, end_s = str(raw).partition(":")

    def one(text, default):
        if text == "":
            return default
        try:
            n = int(text)
        except ValueError:
            raise ValueError("--seq wants a number or a range, got %r" % raw)
        if n == 0:
            raise ValueError("--seq counts sequence numbers from 1, or from -1 "
                             "backwards; got 0")
        return last + 1 + n if n < 0 else n

    return one(start_s, 1), one(end_s, last)


GLOBAL_FLAGS = ("--server", "--home", "--key")


def split_global(argv):
    """Global options come before the command, exactly as in the real client.

    polity now runs every call with `--home` on its own client home
    (SPEC.md:96-97), so a fake that assumed `argv[0]` was the command would
    report "unknown command --home" and hide the wiring instead of testing it.
    """
    i, opts = 0, {}
    while i < len(argv) and argv[i] in GLOBAL_FLAGS:
        opts[argv[i][2:]] = argv[i + 1] if i + 1 < len(argv) else ""
        i += 2
    return argv[i], argv[i + 1:], opts


def main(argv):
    name, rest, opts = split_global(argv[1:])   # argv[0] is this script
    if not name:
        print("no command", file=sys.stderr)
        return 2
    args = args_of(rest)
    # `FAKE_CLI_ARGV` makes the global flags observable: a test can then prove the
    # court ran the client with a home of its own, not the user's.
    log = os.environ.get("FAKE_CLI_ARGV")
    if log:
        with open(log, "a", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps({"cmd": name, "global": opts}, ensure_ascii=False) + "\n")
    state = load()
    print("gongga · offline — connecting…")  # banner noise; not JSON
    emit({"t": "cmd", "id": 1, "name": name, "args": args})

    if name == "set_session":
        session = args.get("session")
        if not session:
            emit({"t": "cmd_result", "id": 1, "ok": False,
                  "message": "set_session is missing --session"})
            return 1
        known = state["sessions"].get(session)
        if known is None:
            known = {
                "created_at": state.get("next_created", 1),
                "rows": [],
                "status": os.environ.get("FAKE_CREATED_STATUS", "idle"),
                "cwd": args.get("cwd", ""),
            }
            state["sessions"][session] = known
            state["next_created"] = known["created_at"] + 1
        emit({"t": "sessions", "sessions": summary(state)})
        save(state)
        emit({"t": "cmd_result", "id": 1, "ok": True,
              "message": "session is now %s (llm fake/model)" % session,
              "data": {"compact_threshold": None, "cwd": known.get("cwd", ""),
                       "llm": "fake/model", "session": session,
                       "created_at": known["created_at"]}})
        return 0

    if name == "get_messages":
        session = args.get("session")
        known = state["sessions"].get(session)
        if known is None:
            emit({"t": "cmd_result", "id": 1, "ok": False,
                  "message": "no session named %r" % session})
            return 1
        # The real client takes `--seq <a:b>` and nothing else (SPEC.md:53).
        # Rejecting the old spelling here is what makes a regression in
        # polity.rows() show up as a red test instead of a silent pass.
        for stale in ("after_seq", "limit"):
            if stale in args:
                emit({"t": "cmd_result", "id": 1, "ok": False,
                      "message": "get_messages has no argument --%s; "
                                 "it takes --session, --seq" % stale})
                return 1
        last = len(known["rows"])
        try:
            first, end = parse_seq(args.get("seq"), last)
        except ValueError as exc:
            emit({"t": "cmd_result", "id": 1, "ok": False, "message": str(exc)})
            return 1
        rows = [r for r in known["rows"] if first <= r["seq"] <= end]
        emit({"t": "sessions", "sessions": summary(state)})
        emit({"t": "cmd_result", "id": 1, "ok": True,
              "message": "%d row(s) at seq %s:%s in %s (transcript ends at seq %d)"
                         % (len(rows), first, end, session, last),
              "data": {"last_seq": last, "messages": rows, "session": session}})
        return 0

    if name == "send_message":
        session = args.get("session")
        known = state["sessions"].get(session)
        if known is None:
            emit({"t": "cmd_result", "id": 1, "ok": False,
                  "message": "send_message requires --session; set a sticky one"})
            return 1
        known["rows"].append({"seq": len(known["rows"]) + 1, "kind": "text",
                              "role": "user", "text": args.get("message", "")})
        reply = os.environ.get("FAKE_REPLY")
        if reply:
            known["rows"].append({"seq": len(known["rows"]) + 1, "kind": "text",
                                  "role": "assistant", "text": reply})
        known["status"] = os.environ.get("FAKE_STATUS", "idle")
        save(state)
        emit({"t": "cmd_result", "id": 1, "ok": True,
              "message": "message accepted by %s" % session})
        return 0

    emit({"t": "cmd_result", "id": 1, "ok": False,
          "message": 'unknown command "%s"' % name})
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
