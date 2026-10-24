#!/usr/bin/env python3
"""Read and fold a gongga session's transcript, straight from the database.

Compaction is a decision about work only the agent has seen, so the server does
not take it: it measures the context and says so (`Context notice`), and this
script is how the agent acts on it.

    transcript.py read     [--session S] [--after SEQ] [--limit N] [--preview]
    transcript.py collapse --session S --upto SEQ --summary TEXT

Reading returns what a conversation is made of -- the model's own messages and
anything injected into the session. Tool calls, tool results and reasoning are
left out: they are most of the rows and least of the meaning.
"""

import argparse
import json
import os
import sqlite3
import sys

DEFAULT_LIMIT = 40
PREVIEW_CHARS = 120
# The server writes are short but frequent; give a racing write time to clear
# instead of failing the one operation that must not fail halfway.
BUSY_TIMEOUT_MS = 5000


def die(message):
    print(message, file=sys.stderr)
    sys.exit(1)


def connect():
    # The server exports its home into the shell; a session running elsewhere
    # is already a mistake worth naming rather than guessing at.
    home = os.environ.get("GONGGA_HOME")
    if not home:
        die(
            "GONGGA_HOME is not set. This tool edits the running server's database, so it "
            "only works from a shell that server started; check with: "
            "echo \"$GONGGA_HOME\" (and $GONGGA_SERVER for the URL)"
        )
    path = os.path.join(home, "gongga.db")
    if not os.path.exists(path):
        die(f"no database at {path}")
    conn = sqlite3.connect(path, timeout=BUSY_TIMEOUT_MS / 1000)
    conn.row_factory = sqlite3.Row
    conn.execute(f"pragma busy_timeout = {BUSY_TIMEOUT_MS}")
    return conn


def session_id(conn, name):
    row = conn.execute("select id from sessions where name = ?", (name,)).fetchone()
    if row is None:
        known = [r["name"] for r in conn.execute("select name from sessions")]
        die(f"no session named {name!r}; this home has: {', '.join(known) or 'none'}")
    return row["id"]


def body(row):
    """The text of a stored message row, as `get_messages` renders it."""
    try:
        value = json.loads(row["content"])
    except ValueError:
        return ""
    return value.get("text") or value.get("summary") or ""


def cmd_read(args):
    conn = connect()
    sid = session_id(conn, args.session)
    summary, upto = conn.execute(
        "select summary, summarised_upto from sessions where id = ?", (sid,)
    ).fetchone()
    after = upto if args.after is None else args.after

    rows = conn.execute(
        """select seq, kind, content from messages
            where session_id = ? and kind = 'text' and seq > ?
            order by seq""",
        (sid, after),
    ).fetchall()
    # Only the tail: the newest messages are where a cut gets chosen.
    rows = rows[-args.limit :] if args.limit else rows

    messages = []
    for row in rows:
        text = body(row)
        role = "user"
        try:
            role = json.loads(row["content"]).get("role") or "user"
        except ValueError:
            pass
        shown = text
        if args.preview:
            flat = text.replace("\n", " ").replace("\r", " ")
            shown = flat[:PREVIEW_CHARS]
            if len(flat) > PREVIEW_CHARS:
                shown += f"... ({len(text)} chars)"
        messages.append(
            {"seq": row["seq"], "role": role, "chars": len(text), "text": shown}
        )

    folded = conn.execute(
        "select count(*) from messages where session_id = ? and kind = 'text' and seq <= ?",
        (sid, upto),
    ).fetchone()[0]
    live = conn.execute(
        "select count(*) from messages where session_id = ? and kind = 'text' and seq > ?",
        (sid, upto),
    ).fetchone()[0]
    last = conn.execute(
        "select coalesce(max(seq), 0) from messages where session_id = ?", (sid,)
    ).fetchone()[0]

    print(
        json.dumps(
            {
                "session": args.session,
                "summary": summary,
                "summary_upto": upto,
                "last_seq": last,
                "folded_messages": folded,
                "live_messages": live,
                "shown_messages": len(messages),
                "messages": messages,
            },
            ensure_ascii=False,
            indent=2,
        )
    )


def cmd_collapse(args):
    if not args.summary.strip():
        die(
            "refusing to fold with an empty summary: that removes this stretch from every "
            "later request while the transcript still looks complete"
        )
    conn = connect()
    sid = session_id(conn, args.session)
    _, previous_upto = conn.execute(
        "select summary, summarised_upto from sessions where id = ?", (sid,)
    ).fetchone()
    if args.upto < previous_upto:
        die(
            f"--upto {args.upto} is behind the current summary (which covers through "
            f"{previous_upto}); folding backwards would put already-folded rows back into "
            "every request"
        )
    last = conn.execute(
        "select coalesce(max(seq), 0) from messages where session_id = ?", (sid,)
    ).fetchone()[0]
    if args.upto > last:
        die(f"--upto {args.upto} is past the last row ({last}); nothing folded")

    # The API rejects a function_call_output whose function_call is absent, so the
    # first row that stays live must be able to start a request on its own.
    first_live = conn.execute(
        """select seq, kind from messages
            where session_id = ? and seq > ? order by seq limit 1""",
        (sid, args.upto),
    ).fetchone()
    if first_live is not None and first_live["kind"] == "tool_result":
        safe = conn.execute(
            """select seq from messages
                where session_id = ? and seq < ? and kind = 'text'
                order by seq desc limit 1""",
            (sid, first_live["seq"]),
        ).fetchone()
        where = f"through {safe['seq']}" if safe else "a message before it"
        die(
            f"--upto {args.upto} would leave a tool result (seq {first_live['seq']}) without "
            f"the call that produced it, and the provider rejects that. Fold {where} instead."
        )

    folded = conn.execute(
        "select count(*) from messages where session_id = ? and seq <= ?", (sid, args.upto)
    ).fetchone()[0]
    live = conn.execute(
        "select count(*) from messages where session_id = ? and seq > ?", (sid, args.upto)
    ).fetchone()[0]

    with conn:
        conn.execute(
            "update sessions set summary = ?, summarised_upto = ? where id = ?",
            (args.summary, args.upto, sid),
        )
        # The provider's last measurement described the prefix just folded, so it
        # says nothing about the next request. Leaving it alone makes the server
        # think the context is still full and remind on every single step.
        conn.execute("update sessions set context_tokens = 0 where id = ?", (sid,))

    print(
        f"folded {folded} row(s) through seq {args.upto} into a "
        f"{len(args.summary)} character summary; {live} row(s) still sent"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    read = sub.add_parser("read", help="show the messages that are actually sent")
    read.add_argument("--session", required=True)
    read.add_argument("--after", type=int, help="defaults to the end of the summary; 0 reaches folded history")
    read.add_argument("--limit", type=int, default=DEFAULT_LIMIT)
    read.add_argument("--preview", action="store_true", help="one line per message")
    read.set_defaults(func=cmd_read)

    collapse = sub.add_parser("collapse", help="fold everything through --upto into --summary")
    collapse.add_argument("--session", required=True)
    collapse.add_argument("--upto", type=int, required=True)
    collapse.add_argument("--summary", required=True)
    collapse.set_defaults(func=cmd_collapse)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
