#!/usr/bin/env python3
"""A server script: it registers commands with gongga-server, no Rust needed.

Copy this file into the server's scripts directory (`$GONGGA_HOME/scripts/`),
then restart the server, or just reconnect: the catalog is re-read when the
scripts directory changes.

One JSON request on stdin, one JSON reply on stdout:

    {"op": "manifest"}                 -> {"commands": [...]}
    {"op": "run", "command": ..., "args": {...}, "home": ..., "server": ...}
                                       -> {"ok": true, "message": ..., "data": ...}

The server puts the harness's own command first on PATH and passes
GONGGA_SERVER and GONGGA_HOME, so a script drives it through the same door the
model uses: `gongga <command> ...`, by name.
"""

import json
import os
import subprocess
import sys


def manifest():
    return {
        "commands": [
            {
                "name": "script_echo",
                "description": "Say back what was passed, from a script.",
                # The same argument shape every other command uses: the server
                # validates it and every client completes it.
                "args": [
                    {
                        "name": "message",
                        "description": "What to echo.",
                        "required": True,
                        "value_hint": "text",
                    },
                ],
            },
            {
                "name": "script_say",
                "description": "Send a message through the CLI, from a script.",
                "args": [
                    {
                        "name": "message",
                        "description": "What to say.",
                        "required": True,
                        "value_hint": "text",
                    },
                    {
                        "name": "session",
                        "description": "Which session hears it.",
                        "required": True,
                        "completions": "sessions",
                    },
                ],
            },
        ]
    }


def cli(*args):
    """Runs the harness CLI and returns its JSON events, one per line.

    The server puts `gongga` on PATH and GONGGA_SERVER in the environment, so
    this needs no flags and no path: the same door the model uses.
    """
    out = subprocess.run(
        ["gongga", *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or f"the CLI left with {out.returncode}")
    return [json.loads(line) for line in out.stdout.splitlines() if line.strip()]


def run(command, args):
    if command == "script_echo":
        return {
            "ok": True,
            "message": args["message"],
            "data": {"args": args, "home": os.environ.get("GONGGA_HOME")},
        }
    if command == "script_say":
        cli("send_message", "--session", args["session"], "--message", args["message"])
        return {"ok": True, "message": f"said it to {args['session']}"}
    return {"ok": False, "message": f"{command} is not mine"}


def main():
    request = json.load(sys.stdin)
    if request.get("op") == "manifest":
        reply = manifest()
    elif request.get("op") == "run":
        try:
            reply = run(request["command"], request.get("args") or {})
        except Exception as e:  # a traceback on stdout would not parse
            reply = {"ok": False, "message": str(e)}
    else:
        reply = {"ok": False, "message": f"unknown op {request.get('op')!r}"}
    print(json.dumps(reply))


if __name__ == "__main__":
    main()
