"""Run a skill's entry: the callable its package publishes.

A skill may be a Python package -- a directory with `__init__.py` -- instead of a
script with a declared entry. Its public API is scanned for the callable to run:
the first name in `__all__` that is callable, else a `main` attribute. That
callable takes one argument, the argv list, and answers with

    None            ran, nothing to print
    a string        ran, and this is what to print
    an int          the exit code
    an exception    it failed, and the traceback is the answer

This file is the contract, not a copy of one. A Python caller that wants a skill
in-process imports it --

    import gongga_skill
    gongga_skill.run("/path/to/skill", ["fetch", "https://example.com"])

-- and it is also what the harness runs when it starts that skill as a process:
`python -c "<this file>" <skill dir> <argv...>`. Nothing here looks for skills
and nothing here knows the roots: a caller that has a name resolves it first,
because "which file does this name open" is one answer and it is not Python's.
"""

import importlib.util
import os
import sys
import traceback

#: A skill that is a package, and the file that makes it one.
PACKAGE = "__init__.py"


def package(directory):
    """The module a skill directory is, or None when it is not a package."""
    init = os.path.join(directory, PACKAGE)
    if not os.path.isfile(init):
        return None
    name = os.path.basename(os.path.normpath(directory)) or "skill"
    spec = importlib.util.spec_from_file_location(
        name, init, submodule_search_locations=[directory]
    )
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def entry_of(module):
    """The callable a package publishes: `__all__` first, then a bare `main`."""
    for name in getattr(module, "__all__", None) or ():
        candidate = getattr(module, name, None)
        if callable(candidate):
            return candidate
    candidate = getattr(module, "main", None)
    return candidate if callable(candidate) else None


def run(directory, argv):
    """Run the skill at `directory` with `argv`, and answer as the contract says."""
    module = package(directory)
    if module is None:
        raise RuntimeError(f"{directory} is not a Python package: no {PACKAGE}")
    entry = entry_of(module)
    if entry is None:
        raise RuntimeError(
            f"{directory} publishes no callable: name one in __all__, or define main(argv)"
        )
    answer = entry(list(argv))
    if answer is None or isinstance(answer, str):
        return answer
    if isinstance(answer, int) and not isinstance(answer, bool):
        return answer
    raise TypeError(
        f"{directory}'s entry answered {type(answer).__name__}: "
        "a skill returns None, a string, or an exit code"
    )


def main(argv):
    """`python gongga_skill.py <skill dir> <argv...>`: the process form of `run`."""
    if not argv:
        print("gongga_skill: name a skill directory", file=sys.stderr)
        return 2
    directory, rest = argv[0], argv[1:]
    try:
        answer = run(directory, rest)
    except Exception:
        traceback.print_exc()
        return 1
    if isinstance(answer, str):
        sys.stdout.write(answer)
        sys.stdout.flush()
        return 0
    return 0 if answer is None else answer


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
