#!/usr/bin/env python3
"""Logic tests for the Windows backend that do not need a live desktop.

The interesting behaviour here is what happens when a window spec is ambiguous or
missing: refusing loudly is the contract every backend shares, and it is the thing
that stops a click from landing in the wrong window.

    python3 test_win.py        # exits non-zero on failure
"""

import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

WINDOWS = [
    {"hwnd": 100, "pid": 1, "title": "scratch.txt - Notepad", "x": 0, "y": 0, "w": 10, "h": 10},
    {"hwnd": 200, "pid": 2, "title": "main.rs - Visual Studio Code", "x": 5, "y": 5, "w": 9, "h": 9},
    {"hwnd": 300, "pid": 3, "title": "other.rs - Visual Studio Code", "x": 7, "y": 7, "w": 3, "h": 3},
]


def load():
    if sys.platform != "win32":
        return None
    spec = importlib.util.spec_from_file_location("cuwin", os.path.join(HERE, "cuwin.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    mod.list_windows = lambda: list(WINDOWS)
    return mod


def check_exact_id(mod):
    assert mod.resolve_window("100")["title"] == "scratch.txt - Notepad"
    assert mod.resolve_window(200)["pid"] == 2, "an int spec must work like its string form"


def check_missing(mod):
    for spec in ("999", "no-such-window"):
        try:
            mod.resolve_window(spec)
        except LookupError as exc:
            assert "cu windows" in str(exc), "the error must say where to look"
            continue
        raise AssertionError("%r should not have resolved" % spec)


def check_ambiguous(mod):
    # Two windows match "Visual Studio Code": guessing would be a coin flip.
    try:
        mod.resolve_window("Visual Studio Code")
    except LookupError as exc:
        message = str(exc)
        assert "ambiguous" in message, message
        assert "300" in message and "200" in message, "both candidates must be listed"
        return
    raise AssertionError("an ambiguous spec must not resolve")


def check_single_match(mod):
    assert mod.resolve_window("Notepad")["hwnd"] == 100, "a unique fragment must resolve"


def check_id_display(mod):
    # Windows hwnds are shown in decimal; only X11 has a hexadecimal convention.
    assert not hasattr(mod, "format_id"), \
        "cuwin must not define format_id, or cu windows would print hex hwnds"


def main():
    mod = load()
    if mod is None:
        print("skipped: the Windows backend can only be imported on Windows")
        return 0
    checks = [check_exact_id, check_missing, check_ambiguous, check_single_match, check_id_display]
    failed = 0
    for check in checks:
        try:
            check(mod)
            print("ok   %s" % check.__name__)
        except Exception as exc:  # noqa: BLE001
            failed += 1
            print("FAIL %s: %s: %s" % (check.__name__, type(exc).__name__, exc))
    print("%d/%d checks passed" % (len(checks) - failed, len(checks)))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
