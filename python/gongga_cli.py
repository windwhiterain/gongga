"""The command line, in Python: one command per process.

    gongga_cli.py <name> [--argument value | --argument=value | bare word ...]

What the server publishes is the whole interface, so nothing here is a table of
commands: the greeting's catalog says which names exist, which arguments each takes,
which are required, and which are spelled without `--`. A word with no flag fills the
first argument still empty from the line's cursor on -- the rule the Rust layer binds
by, because two command lines that disagree about which argument a word lands on are
one line too many.

Events go out one per line as JSON, in the order the server sent them: a script or a
pipeline reads its answer there. The command's own message goes to stderr, and the
exit status is the command's: `0` ran, `1` the server refused, `2` the line itself was
wrong, `3` nothing answered.

Its own commands -- `serve`, the sticky/render pair, `connect_server`, the local
`skill` -- are refused by name rather than guessed at: they are the command layer's
state and its disk, not a server's answer. See `docs/notes/python-cli.md`.
"""

import json
import os
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


def after_arg(specs, name):
    """The cursor a parameter leaves behind: one past it."""
    for at, spec in enumerate(specs):
        if spec["name"] == name:
            return at + 1
    return None


def next_unfilled(specs, bound, cursor):
    """Where a bare word at `cursor` lands: the first unfilled one, else the first.

    The wrap is what keeps a flag ahead of the line from stranding what is behind
    it, and it is the rule the Rust layer binds by.
    """
    order = list(range(cursor, len(specs))) + list(range(0, min(cursor, len(specs))))
    for at in order:
        if specs[at]["name"] not in bound:
            return at
    return None


def bind(wanted, words):
    """The words as `wanted`'s arguments: `--name value`, or a word by position."""
    specs = wanted.get("args") or []
    takes = {spec["name"] for spec in specs}
    values, cursor, i = {}, 0, 0
    while i < len(words):
        word = words[i]
        if word.startswith("--"):
            name, separator, inline = word[2:].partition("=")
            if name not in takes:
                raise ValueError(
                    f"{wanted['name']} has no argument --{name}; it takes "
                    + (", ".join(takes) or "nothing")
                )
            if not separator:
                i += 1
                if i >= len(words):
                    raise ValueError(f"--{name} needs a value")
                inline = words[i]
            if name in values:
                raise ValueError(f"--{name} was given twice")
            values[name] = inline
            cursor = after_arg(specs, name) or cursor
            i += 1
            continue
        at = next_unfilled(specs, values, cursor)
        if at is None:
            raise ValueError(
                f"unexpected argument {word!r}; {wanted['name']} reads " + usage(wanted)
            )
        values[specs[at]["name"]] = word
        cursor = at + 1
        i += 1
    missing = [
        spec["name"] for spec in specs if spec.get("required") and spec["name"] not in values
    ]
    if missing:
        raise ValueError(
            f"{wanted['name']} needs " + ", ".join(missing) + "; it reads " + usage(wanted)
        )
    return values


#: The flags that come before the command's name: they say which home and which
#: server, and they are this program's, never the command's.
GLOBALS = {"--home": "GONGGA_HOME", "--server": "GONGGA_SERVER", "--key": "GONGGA_KEY"}


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
        settings[GLOBALS[name]] = inline
        i += 1
    return settings, words[i:]


def catalog(client):
    """What this server publishes, one command a line: the answer to bare invocation."""
    for row in client.catalog:
        print(usage(row))
        if row.get("description"):
            print("    " + row["description"].replace("\n", " "))
    return 0


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
    if not argv:
        client = dial()
        if client is None:
            return 3
        try:
            return catalog(client)
        finally:
            client.close()
    name, words = argv[0], argv[1:]
    client = dial()
    if client is None:
        return 3
    try:
        wanted = next((row for row in client.catalog if row["name"] == name), None)
        # A name the layer answers itself is refused only when this server does not
        # publish it too: `skill` is both, and the server's is the one that runs here.
        if wanted is None and name in CLIENT_SIDE:
            print(
                f"gongga-cli: {name} {CLIENT_SIDE[name]}, which the Python command line "
                "does not answer yet",
                file=sys.stderr,
            )
            return 2
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
    # The command's answer is the line a reader (or a pipe) takes away, so it goes to
    # stdout with the events: stderr is for what this program says itself.
    if reply.message:
        print(reply.message)
    return 0 if reply.ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
