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
import re
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
        hint = argument.get("value_hint") or argument["name"]
        word = f"--{argument['name']} <{hint}>"
        if argument.get("positional"):
            word = f"<{hint}>"
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
                    f"{wanted['name']} has no argument --{name}; it reads " + usage(wanted)
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
                f'unexpected argument "{word}"; {wanted["name"]} reads ' + usage(wanted)
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
        settings[GLOBALS[name]] = native(inline) if name == "--home" else inline
        i += 1
    return settings, words[i:]


def catalog(client):
    """What this server publishes: the answer to a bare invocation.

    # Invariant

    The list is the greeting's and the shape is the layer's: `<name> -- <what it
    is>`, then `takes:` on its own line. Who runs a command is the reader's
    business, so a client command and a script's say so and a server's says nothing.
    """
    rows = client.catalog
    print(
        f"{len(rows)} command(s); run one with `gongga <command> <args>`; each takes "
        "line says its shape."
    )
    for row in rows:
        who = {"client": " [client-side]", "server_script": " [script]"}
        print(f"{row['name']}{who.get(row.get('target') or 'server', '')} -- {row.get('description', '')}")
        print("    takes: " + usage(row))
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
    # Every row of the answer, the reply line last: what the server said, in the order
    # it said it, so a caller that reads lines sees the events and the verdict as one
    # stream -- the shape `--json` prints and a script parses.
    for event in reply.events:
        # Compact, one row a line: the shape the layer has always printed, so a
        # script that greps `"ok":true` keeps reading what it read before.
        print(json.dumps(event, ensure_ascii=False, separators=(",", ":")))
    return 0 if reply.ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
