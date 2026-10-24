#!/usr/bin/env python3
"""Parse-level tests for the Linux backend, runnable on any OS.

`culinux.py` shells out to wmctrl, xdotool and xwininfo, so the parts that can be
wrong without a Linux machine are its parsers. Each case below is real tool output
and each assertion is a defect that was actually found this way, not a hypothetical.

What this does not cover: that the tools emit exactly these shapes on a given
distro. That needs a real X11 session, and `culinux.py` says so in its docstring.

    python3 test_parsers.py        # exits non-zero on failure
"""

import importlib.util
import os
import sys
import types

HERE = os.path.dirname(os.path.abspath(__file__))


def load():
    spec = importlib.util.spec_from_file_location("culinux", os.path.join(HERE, "culinux.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def fake_run(payload, rc=0):
    return lambda cmd: types.SimpleNamespace(returncode=rc, stdout=payload, stderr="")


def fixture(cul, *, wmctrl=None, xwininfo=None, session="x11"):
    """Install mocks for exactly the tools this check needs, and nothing else.

    Checks used to bind ``_run`` directly and leak into each other: a later check
    then fed xwininfo output to the wmctrl parser and failed for the wrong reason.
    """
    def run(cmd):
        if cmd[0] == "wmctrl" and wmctrl is not None:
            return types.SimpleNamespace(returncode=0, stdout=wmctrl, stderr="")
        if cmd[0] == "xwininfo" and xwininfo is not None:
            return types.SimpleNamespace(returncode=0, stdout=xwininfo, stderr="")
        return types.SimpleNamespace(returncode=1, stdout="", stderr="unmocked: %s" % cmd[0])

    cul._run = run
    cul._have = lambda tool: (tool == "wmctrl" and wmctrl is not None) or \
                            (tool == "xwininfo" and xwininfo is not None)
    cul._session_type = lambda: session
    os.environ["XDG_SESSION_TYPE"] = session


# `wmctrl -lGp`: id, desktop, pid, x, y, w, h, then "<client-machine> <title>".
WMCTRL = """\
0x01000007  0    1234  0    0    1920 1080 admin-pc 桌面 - 文件管理器
0x01200005  0    5678  100  50   800  600  admin-pc Terminal — bash
0x0140000a -1    999   0    0    1920  24   admin-pc panel
0x01600002  1    4321  10   10   400  300  admin-pc Title   with   spaces
0x000004d2  1    7777  0    0    10   10   admin-pc Decimal Trap
"""

# `xwininfo -tree`: indentation is the only nesting it encodes, and a negative
# coordinate prints as "+-10".
XWINFO = """\
xwininfo: Window id: 0x1200005 "Term"
  Root window id: 0x2ff (the root window) (has no name)
     2 children:
     0x1200006 "outer": ("a" "A")  800x600+100+50  +100+50
      0x1200007 "inner": ("b" "B")  200x100+-10+-20  +90+30
     0x1200008 "sibling": ("c" "C")  10x10+0+0  +0+0
"""


def check_windows(cul):
    fixture(cul, wmctrl=WMCTRL)
    wins = cul.list_windows()

    assert len(wins) == 4, "the desktop -1 panel must be filtered out, got %d" % len(wins)
    assert all(w["pid"] != 999 for w in wins), "sticky panel survived the filter"
    # The client machine prefixes field 8; leaving it on breaks matching by title.
    assert wins[0]["title"] == "桌面 - 文件管理器", repr(wins[0]["title"])
    assert wins[1]["title"] == "Terminal — bash", repr(wins[1]["title"])
    assert wins[2]["title"] == "Title   with   spaces", "inner spacing must survive"
    assert wins[1]["hwnd"] == 0x01200005 and wins[1]["pid"] == 5678
    assert (wins[1]["x"], wins[1]["y"], wins[1]["w"], wins[1]["h"]) == (100, 50, 800, 600)


def check_resolve(cul):
    fixture(cul, wmctrl=WMCTRL)
    # Both spellings of the same id must land on the same window.
    assert cul.resolve_window("0x01200005")["pid"] == 5678, "hex id must resolve"
    assert cul.resolve_window(str(0x01200005))["pid"] == 5678, "decimal id must resolve"
    # A bare number is decimal, never a surprise hex: reading "1234" as 0x1234
    # would pick the "Decimal Trap" window instead of the one with id 1234.
    assert cul.resolve_window("1234")["title"] == "Decimal Trap", \
        "bare number must read as decimal, got %r" % cul.resolve_window("1234")["title"]
    assert cul.resolve_window("Terminal")["pid"] == 5678, "title fragment must resolve"
    # What `cu windows` prints has to be accepted back verbatim.
    shown = cul.format_id(0x01200005)
    assert shown == "0x01200005", shown
    assert cul.resolve_window(shown)["pid"] == 5678, "displayed id must round-trip"
    for spec in ("admin-pc", "nothing-here"):
        try:
            cul.resolve_window(spec)
        except LookupError:
            continue
        raise AssertionError("%r should not resolve, or it resolved to the wrong window" % spec)


def check_tree(cul):
    fixture(cul, wmctrl=WMCTRL, xwininfo=XWINFO)
    header, lines = cul.tree("0x01200005", depth=3, limit=20)

    assert ("id=%s" % cul.format_id(0x01200005)) in header, header
    body = [l for l in lines if l.startswith(" ")]
    named = {l.split('"')[1]: l for l in body}
    assert set(named) == {"outer", "inner", "sibling"}, set(named)
    # The name sits before the parenthesised class; splitting on "(" yields the id.
    assert "0x1200006" not in "".join(body), "window id leaked into the name column"
    # "+-10" is -10, and must render like every other backend so a centre can be
    # computed from the line without special cases.
    assert "@-10,-20" in named["inner"], named["inner"]
    assert "@100,50" in named["outer"], named["outer"]
    assert not any("+-" in l or "@+" in l for l in body), "sign leaked into coordinates"
    # Indentation carries nesting, so a child has to sit deeper than its parent.
    depth = lambda l: (len(l) - len(l.lstrip(" ")))
    assert depth(named["inner"]) > depth(named["outer"]), "nesting lost"


def check_wayland_refuses(cul):
    fixture(cul, wmctrl=WMCTRL, xwininfo=XWINFO, session="wayland")
    try:
        cul.tree("0x01200005")
    except RuntimeError as exc:
        assert "Wayland" in str(exc), str(exc)
        return
    raise AssertionError("the window tree must refuse to claim X11 structure under Wayland")


def main():
    cul = load()
    checks = [check_windows, check_resolve, check_tree, check_wayland_refuses]
    failed = 0
    for check in checks:
        try:
            check(cul)
            print("ok   %s" % check.__name__)
        except Exception as exc:  # noqa: BLE001 - a parser that raises is a failure
            # A backend that raises instead of returning a bad value is still a
            # failure; reporting it as a traceback hides it from the pass/fail line.
            failed += 1
            print("FAIL %s: %s: %s" % (check.__name__, type(exc).__name__, exc))
    print("%d/%d checks passed" % (len(checks) - failed, len(checks)))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
