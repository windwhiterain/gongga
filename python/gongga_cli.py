"""The command line, in Python: one command per process.

    gongga_cli.py <name> [--argument value | --argument=value | bare word ...]

What the server publishes is the whole interface, so nothing here is a table of
commands: the greeting's catalog says which names exist, which arguments each takes,
which of them are required, and which take a bare word instead of `--name`.

One event per line on stdout, JSON, exactly as the server sent it -- the shape a
script or a pipeline reads. The command's own message goes to stderr, and the exit
status is the command's: `0` ran, `1` the server refused, `2` the line itself was
wrong, `3` nothing answered.

What is *not* here yet, and is deliberately refused rather than guessed: the client's
own commands (`set_sticky`, `set_render`, `connect_server`, `serve`, and the local
`skill`), because they are answered by the command layer's own state and not by a
server. See `docs/notes/python-cli.md`.
"""

import json
import os
import socket
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import gongga

#: The commands a client answers itself, in the Rust layer. Named here only to refuse
#: them with the right words: a second implementation would be a second answer.
CLIENT_SIDE = {
    "serve": "starts a server of its own",
    "set_sticky": "is the window's state",
    "del_sticky": "is the window's state",
    "set_render": "is the window's state",
    "set_render_session": "is the window's state",
    "connect_server": "is the window's state",
    "skill": "runs an entry on this disk",
}


def usage(spec):
    """One command's shape, as the catalog publishes it."""
    words = [spec["name"]]
    for argument in spec.get("args") or []:
        word = f"--{argument['name']} <{argument['name']}>"
        if argument.get("positional"):
            word = f"<{argument['name']}>"
        if not argument.get("required"):
            word = f"[{word}]"
        words.append(word)
    return " ".join(words)


def bind(wanted, words):
    """The words as `spec`'s arguments, by position and by `--name`."""
    specs = wanted.get("args") or []
    named = {spec["name"]: spec for spec in specs}
    # Which of them take a bare word, in the order published.
    positional = [spec["name"] for spec in specs if spec.get("positional")]
    values, rest = {}, []
    i = 0
    while i < len(words):
        word = words[i]
        if word.startswith("--"):
            name, separator, inline = word[2:].partition("=")
            if name not in named:
                raise ValueError(
                    f"{name} is not an argument of {wanted['name']}; it takes "
                    + (", ".join(named) or "nothing")
                )
            if not separator:
                i += 1
                if i >= len(words):
                    raise ValueError(f"--{name} needs a value")
                inline = words[i]
            values[name] = inline
        else:
            rest.append(word)
        i += 1
    for name in positional:
        if not rest:
            break
        values.setdefault(name, rest.pop(0))
    if rest:
        raise ValueError("more words than the command takes: " + " ".join(rest))
    missing = [
        spec["name"]
        for spec in specs
        if spec.get("required") and spec["name"] not in values
    ]
    if missing:
        raise ValueError(
            f"{wanted['name']} needs " + ", ".join(missing) + "; it reads "
            + usage(wanted)
        )
    return values


def catalog(client):
    """What this server publishes, one line each: the answer to a bare invocation."""
    for row in client.catalog:
        print(usage(row))
        if row.get("description"):
            print("    " + row["description"].replace("\n", " "))
    return 0


def main(argv):
    if not argv:
        try:
            client = gongga.connect()
        except (gongga.Unreachable, FileNotFoundError) as unreachable:
            print(f"gongga-cli: {unreachable}", file=sys.stderr)
            return 3
        try:
            return catalog(client)
        finally:
            client.close()
    name, words = argv[0], argv[1:]
    if name in CLIENT_SIDE:
        print(
            f"gongga-cli: {name} {CLIENT_SIDE[name]}, which the Python command line "
            "does not answer yet",
            file=sys.stderr,
        )
        return 2
    try:
        client = gongga.connect()
    except (gongga.Unreachable, FileNotFoundError) as unreachable:
        print(f"gongga-cli: {unreachable}", file=sys.stderr)
        return 3
    try:
        wanted = next((row for row in client.catalog if row["name"] == name), None)
        if wanted is None:
            print(
                f"gongga-cli: this server publishes no {name!r}; run it with no name "
                "for the ones it does",
                file=sys.stderr,
            )
            return 2
        try:
            arguments = bind(wanted, words)
        except ValueError as refused:
            print(f"gongga-cli: {refused}", file=sys.stderr)
            return 2
        reply = client.call(name, **arguments)
    except gongga.Unreachable as unreachable:
        print(f"gongga-cli: {unreachable}", file=sys.stderr)
        return 3
    finally:
        client.close()
    for event in reply.events:
        print(json.dumps(event, ensure_ascii=False))
    if reply.message:
        print(reply.message, file=sys.stderr)
    return 0 if reply.ok else 1


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except socket.timeout:
        print("gongga-cli: the server stopped answering", file=sys.stderr)
        sys.exit(3)
