"""The command line, in Python: one server Cmd per process.

    gongga_cli.py '{"name":"<cmd>","args":{...}}'

SPEC's `Cmd := {name: str, args: dict[str, any]}`: the rpc layer sends one server
Cmd, checked under the server's schema, and prints what came back. There is no
command-line flag parsing and no client command here: `set_sticky`, `set_render`,
`connect_server`, `serve` are the client's own (SPEC `## Client Cmd`), not a server's
answer. The server is the environment's (`GONGGA_HOME` / `GONGGA_SERVER` /
`GONGGA_KEY`), or the global `--home` / `--server` / `--key` flags ahead of the JSON.

Events go out one per line as JSON, in the order the server sent them. The exit
status is the command's: 0 ran, 1 the server refused, 2 the line itself was wrong,
3 nothing answered.
"""


import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import gongga


GLOBALS = {"--home": "GONGGA_HOME", "--server": "GONGGA_SERVER", "--key": "GONGGA_KEY"}


def native(path):
    r"""A `/c/...` path as this platform spells it.

    The shell this command line is typed into is bash, and bash hands its paths on
    as `/c/...`; a Windows program opens `C:\...`. One conversion, here, for the
    arguments that are paths -- the command's own words are never rewritten.
    """
    if os.name != "nt":
        return path
    drive = re.match(r"^/([A-Za-z])/(.*)$", path)
    if drive:
        return drive.group(1).upper() + ":\\" + drive.group(2).replace("/", "\\")
    return path


def globals_of(words):
    """The words split into this program's flags and the command's own.

    # Invariant

    Only the flags ahead of the command are read: `--home` after a command name is
    that command's argument, and the layer's own words are none of this file's
    business.
    """
    settings, i = {}, 0
    while i < len(words):
        word = words[i]
        name, separator, inline = word.partition("=")
        if name not in GLOBALS:
            break
        if not separator:
            i += 1
            if i >= len(words):
                raise ValueError(f"{name} needs a value")
            inline = words[i]
        settings[GLOBALS[name]] = native(inline) if name == "--home" else inline
        i += 1
    return settings, words[i:]


def dial():
    """The connection, or the reason there is none, said as this program's own words."""
    try:
        return gongga.connect()
    except gongga.Unreachable as unreachable:
        print(f"gongga-cli: {unreachable}", file=sys.stderr)
        return None
    except FileNotFoundError as missing:
        print(f"gongga-cli: no key to dial with: {missing}", file=sys.stderr)
        return None


def main(argv):
    try:
        settings, argv = globals_of(argv)
    except ValueError as refused:
        print(f"gongga-cli: {refused}", file=sys.stderr)
        return 2
    os.environ.update(settings)
    # SPEC's Cmd := {name, args}: one JSON object, no command-line flags.
    if len(argv) != 1:
        print('gongga-cli reads one JSON object: {"name":"<cmd>","args":{...}}', file=sys.stderr)
        return 2
    try:
        cmd = json.loads(argv[0])
    except ValueError:
        print(f"gongga-cli: not a JSON object: {argv[0]!r}", file=sys.stderr)
        return 2
    if not isinstance(cmd, dict) or not isinstance(cmd.get("name"), str):
        print('gongga-cli: a Cmd is {"name":"<cmd>","args":{...}}', file=sys.stderr)
        return 2
    name, args = cmd["name"], cmd.get("args") or {}
    if not isinstance(args, dict):
        print('gongga-cli: "args" must be an object', file=sys.stderr)
        return 2
    client = dial()
    if client is None:
        return 3
    try:
        reply = client.call(name, **args)
    except gongga.Unreachable as unreachable:
        print(f"gongga-cli: {unreachable}", file=sys.stderr)
        return 3
    finally:
        client.close()
    for event in reply.events:
        print(json.dumps(event, ensure_ascii=False, separators=(",", ":")))
    return 0 if reply.ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
