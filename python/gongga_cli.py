"""The command line, in Python: one command per process.

    gongga_cli.py <name> [--argument value | --argument=value ...]

What the server publishes is the whole interface, so nothing here is a table of
commands: the greeting's catalog says which names exist, which arguments each takes and
which are required. Every one of them is spelled `--name`, including the ones the
catalog marks positional: a bare word on a line nobody is watching is a typo said out
loud, and the rule that guessed which argument it landed on guessed wrong (`send_message
reader hello` sent "hello" to a session called `hello`). So a bare word is refused, and
the refusal names the flags the command does take.

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
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import gongga

#: Where the skills a session can open live, most specific first.
def roots(home):
    """The three directories, as `crates/skill` orders them.

    # Invariant

    This walk is the one place a rule is written twice: the window and the server
    read skills through `crates/skill`, and a command line with no server has no Rust
    to ask. The order, the name and the frontmatter are that crate's -- if one moves,
    both move.
    """
    project = os.path.join(os.getcwd(), ".agents", "skills")
    machine = os.path.join(os.path.expanduser("~"), ".agents", "skills")
    return [("project", project), ("harness", os.path.join(home, "skills")), ("general", machine)]


def describe(directory):
    """A skill's `name` and `description` from its frontmatter, or from its name."""
    name = os.path.basename(os.path.normpath(directory))
    description = ""
    try:
        with open(os.path.join(directory, "SKILL.md"), encoding="utf-8") as handle:
            lines = handle.read().splitlines()
    except OSError:
        return name, description
    if not lines or lines[0].strip() != "---":
        return name, description
    for line in lines[1:]:
        if line.strip() == "---":
            break
        key, _, value = line.partition(":")
        if key.strip() == "name" and value.strip():
            name = value.strip()
        if key.strip() == "description" and value.strip():
            description = value.strip()
    return name, description


def installed(home):
    """Every skill this machine can open, most specific first, one row each."""
    seen, rows = set(), []
    for bucket, directory in roots(home):
        try:
            entries = sorted(os.listdir(directory))
        except OSError:
            continue
        for entry in entries:
            at = os.path.join(directory, entry)
            if not os.path.isfile(os.path.join(at, "SKILL.md")):
                continue
            name, description = describe(at)
            if name in seen:
                continue
            seen.add(name)
            rows.append((name, bucket, description, os.path.join(at, "SKILL.md")))
    return rows


def usage(spec):
    """One command's shape, as the catalog publishes it."""
    words = [spec["name"]]
    for argument in spec.get("args") or []:
        hint = argument.get("value_hint") or argument["name"]
        word = f"--{argument['name']} <{hint}>"
        if not argument.get("required"):
            word = f"[{word}]"
        words.append(word)
    return " ".join(words)


def bind(wanted, words):
    """The words as `wanted`'s arguments: `--name value`, `--name=value`, and no other shape.

    # Invariant

    A word with no dash is refused, never placed. Which argument such a word was meant
    for is a guess, and the guess is what sent `send_message reader hello` to a session
    named `hello`; the line this reads is one nobody can see, so it says what it takes
    instead of deciding for the caller.
    """
    specs = wanted.get("args") or []
    takes = {spec["name"] for spec in specs}
    values, i = {}, 0
    while i < len(words):
        word = words[i]
        if not word.startswith("--"):
            raise ValueError(
                f'{wanted["name"]} takes its arguments by name, and "{word}" names none; '
                "it reads " + usage(wanted)
            )
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
        i += 1
    missing = [
        spec["name"] for spec in specs if spec.get("required") and spec["name"] not in values
    ]
    if missing:
        raise ValueError(
            "missing required argument" + ("s" if len(missing) > 1 else "")
            + ": " + ", ".join(missing) + "; it reads " + usage(wanted)
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


def _spells_the_server(words):
    """Whether these words are the *server's* `skill`, not this disk's.

    # Invariant

    `--name` and `--args` are the server command's own spelling (a session, then the
    skill, then its words as JSON), so their presence decides; everything else is the
    local form, `ls` / `run <name>` / `<name> <args...>`.
    """
    return any(
        word == "--name"
        or word.startswith("--name=")
        or word == "--args"
        or word.startswith("--args=")
        for word in words
    )


def table(reply):
    """The skills a server's `skill` answered with, one tab-separated row each."""
    try:
        rows = json.loads(reply.message).get("skills") or []
    except (ValueError, AttributeError):
        return
    for row in rows:
        print(
            f"{row.get('name', '')}\t{row.get('bucket', '')}\t"
            f"{row.get('description', '')}\t{row.get('path', '')}"
        )


def home_of():
    """This home, as the words and the environment spell it."""
    return native(os.environ.get("GONGGA_HOME") or os.path.join(os.path.expanduser("~"), ".gongga"))


def serve(words):
    """`serve --port <port> [--home <home>]`: hand the process to the server."""
    port, i = None, 0
    while i < len(words):
        word = words[i]
        if word in ("--port", "--home"):
            i += 1
            if i >= len(words):
                print(f"gongga-cli: {word} needs a value", file=sys.stderr)
                return 2
            if word == "--port":
                port = words[i]
            else:
                os.environ["GONGGA_HOME"] = native(words[i])
        elif "--port=" in word:
            port = word.split("=", 1)[1]
        elif port is None and not word.startswith("-"):
            port = word
        i += 1
    if port is None:
        print("gongga-cli: serve needs a port", file=sys.stderr)
        return 2
    home = home_of()
    program = os.path.join(home, "bin", "gongga-server" + (".exe" if os.name == "nt" else ""))
    if not os.path.isfile(program):
        print(f"gongga-cli: no server at {program}", file=sys.stderr)
        return 3
    # The server takes over this process's role, not this process: its own container.
    return subprocess.run([program, "--home", home, "--bind", f"127.0.0.1:{port}"]).returncode


def skill(words):
    """`skill ls` (a table), `skill run <name>`, or `skill <name> [args...]`."""
    home = home_of()
    # A verb first: `ls` is the table, `run` reaches a skill whose own name is `ls`.
    verb, rest = (words[0], words[1:]) if words else ("ls", [])
    if verb == "ls":
        session = os.environ.get("GONGGA_SESSION")
        if session:
            # A shell the server started knows its own checkout, so the table is the
            # one that session would be offered; without it, this disk's.
            client = dial()
            if client is None:
                return 3
            try:
                reply = client.call("skill", session=session)
            finally:
                client.close()
            table(reply)
            return 0 if reply.ok else 1
        for name, bucket, description, path in installed(home):
            print(f"{name}\t{bucket}\t{description}\t{path}")
        return 0
    if verb == "run":
        if not rest:
            print("gongga-cli: run needs a skill name", file=sys.stderr)
            return 2
        verb, rest = rest[0], rest[1:]
    name, argv = verb, rest
    found = [row for row in installed(home) if row[0] == name]
    if not found:
        print(f"gongga-cli: no skill named {name!r} is installed", file=sys.stderr)
        return 2
    if len(found) > 1:
        print(
            f"gongga-cli: {name} is installed more than once: "
            + ", ".join(f"{row[3]} ({row[1]})" for row in found),
            file=sys.stderr,
        )
        return 2
    directory = os.path.dirname(found[0][3])
    package = os.path.isfile(os.path.join(directory, "__init__.py"))
    if package:
        # A package publishes functions: the first word names one.
        driver = os.path.join(home, "lib", "gongga_skill.py")
        return subprocess.run([sys.executable, driver, directory, *argv]).returncode
    declared = _declared(directory)
    if not declared:
        print(
            f"gongga-cli: {name} is a body and nothing to run: read {found[0][3]}",
            file=sys.stderr,
        )
        return 2
    return run_entry(os.path.join(directory, declared), argv)


def run_entry(entry, argv):
    """Run a script entry by its extension, then by its `#!`.

    # Invariant

    `crates/skill/src/entry.rs` is the rule, and this is its second reading: a script
    has no server to ask, and a command line that cannot run one is not a command
    line. Extension first because Windows honours no `#!`.
    """
    suffix = os.path.splitext(entry)[1].lower()
    if suffix in (".sh", ".bash"):
        program = ["bash"]
    elif suffix == ".py":
        program = [sys.executable]
    elif suffix == ".ps1":
        program = [which("pwsh") or which("powershell") or "powershell", "-NoProfile", "-File"]
    elif suffix in (".cmd", ".bat"):
        program = ["cmd", "/c"]
    else:
        program = []
        try:
            with open(entry, encoding="utf-8") as handle:
                first = handle.readline()
        except OSError:
            first = ""
        if first.startswith("#!"):
            words = first[2:].split()
            if words and os.path.basename(words[0]) == "env":
                words = [w for w in words[1:] if not w.startswith("-")]
            if words:
                program = [os.path.basename(words[0])]
    if not program:
        return 2
    return subprocess.run([*program, entry, *argv]).returncode


def which(name):
    """The program `name` on this machine's PATH, or None."""
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        candidate = os.path.join(directory, name)
        for suffix in ("", ".exe", ".cmd", ".bat"):
            if os.path.isfile(candidate + suffix):
                return candidate + suffix
    return None


def _declared(directory):
    """The entry a `SKILL.md` declares under `metadata`, or empty."""
    try:
        with open(os.path.join(directory, "SKILL.md"), encoding="utf-8") as handle:
            lines = handle.read().splitlines()
    except OSError:
        return ""
    inside = False
    for line in lines:
        if line.strip() == "metadata:":
            inside = True
            continue
        if inside:
            if not line.startswith(" "):
                return ""
            key, _, value = line.strip().partition(":")
            if key == "entry" and value.strip():
                return value.strip()
    return ""


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
    # The two commands that are not a server's answer: one starts a server, the other
    # reads this disk.
    if name == "serve":
        return serve(words)
    if name == "skill" and not _spells_the_server(words):
        return skill(words)
    client = dial()
    if client is None:
        return 3
    try:
        wanted = next((row for row in client.catalog if row["name"] == name), None)
        if wanted is None:
            # A window's own commands (`set_sticky`, `set_render`, `connect_server`)
            # arrive here too, and the answer is the same: this process has no state
            # for them, so it has no such command.
            print(
                f'unknown command "{name}"; this server publishes: '
                + ", ".join(row["name"] for row in client.catalog),
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
    if not reply.ok:
        return 1
    # `skill` with no name answers with the table, and the table is four columns:
    # a reader and a model look at one shape, whether the server or this disk said it.
    if name == "skill":
        table(reply)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
