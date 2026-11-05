"""Run a skill's entry: the function its package publishes.

A skill may be a Python package -- a directory with `__init__.py` -- instead of a
script with a declared entry. Its public API is its *functions*, and one of them
is what a call runs. The API is `__all__` when the package states one, and
otherwise the module's own callables whose names do not start with `_`.

    import gongga_skill
    gongga_skill.run("/path/to/skill", "fetch", ["https://example.com"])

The words after the function are bound to its parameters: a word per parameter
in declaration order, and `--name value` (or `--name=value`) to spell one out --
which is also how a word that starts with `--` is passed. An annotation of `int`,
`float` or `bool` converts the word; anything else arrives as written. A parameter
that cannot be filled is refused with the signature, not with a traceback.

What the function answers is what a caller gets:

    None            ran, nothing to print
    a string        ran, and this is what to print
    an int          the exit code
    anything else   printed as JSON, so a skill may answer a structure

This file is the contract, not a copy of one. A Python caller imports it for the
in-process form; the harness carries the same file with `include_str!` and runs
it as `python -c "<this file>" <skill dir> [function] [words...]`. Neither looks
for skills: a caller that has a *name* resolves it first, because "which file
does this name open" is one answer and it is not Python's.
"""

import importlib.util
import inspect
import json
import os
import sys
import traceback

#: The file that makes a skill directory a package.
PACKAGE = "__init__.py"


def _as_bool(word):
    lowered = word.strip().lower()
    if lowered in ("true", "1", "yes", "on"):
        return True
    if lowered in ("false", "0", "no", "off"):
        return False
    raise ValueError(f"{word!r} is not a boolean")


#: Annotations a word is converted to; anything else keeps the word as written.
CONVERT = {int: int, float: float, bool: _as_bool}


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


def functions(module):
    """The public API of a package: name to callable, in declaration order."""
    stated = getattr(module, "__all__", None)
    names = list(stated) if stated is not None else list(vars(module))
    found = {}
    for name in names:
        candidate = getattr(module, name, None)
        if name.startswith("_") or not callable(candidate):
            continue
        if inspect.ismodule(candidate) or inspect.isclass(candidate):
            continue
        found[name] = candidate
    return found


def bind(entry, words):
    """The words as `entry`'s arguments: by position, or spelled `--name value`."""
    signature = inspect.signature(entry)
    parameters = [
        parameter
        for parameter in signature.parameters.values()
        if parameter.kind
        in (
            inspect.Parameter.POSITIONAL_ONLY,
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.VAR_POSITIONAL,
        )
    ]
    # A trailing `*words` takes whatever is left; without one, a word too many is
    # a mistake worth saying, not something to drop.
    rest = parameters[-1] if parameters and parameters[-1].kind == inspect.Parameter.VAR_POSITIONAL else None
    if rest is not None:
        parameters = parameters[:-1]
    allowed = {parameter.name for parameter in parameters}
    positional, keywords = [], {}
    i = 0
    while i < len(words):
        word = words[i]
        if word.startswith("--"):
            name, separator, inline = word[2:].partition("=")
            if name not in allowed:
                raise TypeError(
                    f"{entry.__name__}{signature} has no {name!r}; it takes "
                    + (", ".join(allowed) or "nothing")
                )
            if not separator:
                i += 1
                if i >= len(words):
                    raise TypeError(f"--{name} needs a value")
                inline = words[i]
            keywords[name] = inline
        else:
            if len(positional) >= len(parameters) and rest is None:
                raise TypeError(
                    f"{entry.__name__}{signature} takes {len(parameters)} word(s); "
                    "this call has more"
                )
            positional.append(word)
        i += 1

    filled = set(keywords) | {parameter.name for parameter in parameters[: len(positional)]}
    for parameter in parameters:
        if parameter.name not in filled and parameter.default is parameter.empty:
            raise TypeError(
                f"{entry.__name__}{signature} needs {parameter.name}"
            )

    def converted(name, word):
        convert = CONVERT.get(signature.parameters[name].annotation)
        return convert(word) if convert else word

    args = [converted(parameter.name, word) for parameter, word in zip(parameters, positional)]
    if rest is not None:
        args.extend(positional[len(parameters):])
    kwargs = {name: converted(name, word) for name, word in keywords.items()}
    return args, kwargs


def run(directory, function, words):
    """Run `function` of the skill at `directory` with `words`, and answer."""
    module = package(directory)
    if module is None:
        raise RuntimeError(f"{directory} is not a Python package: no {PACKAGE}")
    public = functions(module)
    skill = os.path.basename(os.path.normpath(directory))
    if not function:
        raise TypeError(f"{skill} publishes: " + (", ".join(public) or "no functions"))
    entry = public.get(function)
    if entry is None:
        raise TypeError(
            f"{skill} has no function {function!r}; it publishes: "
            + (", ".join(public) or "nothing")
        )
    args, keywords = bind(entry, words)
    return entry(*args, **keywords)


def render(answer):
    """What a caller sees of an answer: text, an exit code, or a structure."""
    if answer is None:
        return "", None
    if isinstance(answer, str):
        return answer, None
    if isinstance(answer, int) and not isinstance(answer, bool):
        return "", answer
    return json.dumps(answer, ensure_ascii=False, sort_keys=True), None


def main(argv):
    """`python gongga_skill.py <skill dir> [function] [words...]`: `run` as a process."""
    if not argv:
        print("gongga_skill: name a skill directory", file=sys.stderr)
        return 2
    directory = argv[0]
    rest = argv[1:]
    function = rest[0] if rest and not rest[0].startswith("--") else None
    words = rest[1:] if function else rest
    try:
        text, code = render(run(directory, function, words))
    except (TypeError, RuntimeError, ValueError) as refused:
        # What the caller wrote does not fit the API: said in one line, not as a
        # traceback, which reads as a bug in this file.
        print(f"gongga_skill: {refused}", file=sys.stderr)
        return 2
    except Exception:
        traceback.print_exc()
        return 1
    if text:
        sys.stdout.write(text)
        sys.stdout.flush()
    return 0 if code is None else code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
