"""Linux backend: shell tools, because every display server already ships a driver.

Status: NOT verified on a real Linux desktop. Written from documented behaviour of
xdotool, wmctrl and ImageMagick, never run, because the machine this was written on
is Windows.

X11 and Wayland are not the same target. Under X11 these tools drive the session
directly. Under Wayland a client cannot inject input or read other windows by
design, so `click`/`type` need `ydotool` and the tree degrades to whatever the
compositor exposes. `doctor` says which one it found instead of pretending both
work.
"""

import os
import re
import shutil
import subprocess


def _have(tool):
    return shutil.which(tool) is not None


def _run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True)


def _session_type():
    return os.environ.get("XDG_SESSION_TYPE", "unknown").lower()


def list_windows():
    """`wmctrl -lGp` gives id, desktop, pid, geometry and title in one line."""
    if not _have("wmctrl"):
        raise RuntimeError("wmctrl not installed (apt install wmctrl)")
    found = []
    for line in _run(["wmctrl", "-lGp"]).stdout.splitlines():
        parts = line.split(None, 7)
        if len(parts) < 8:
            continue
        wid, desktop, pid, x, y, w, h, rest = parts
        if desktop == "-1":  # sticky panels and docks: nothing to click in the flow
            continue
        # Field 8 is "client-machine title", not the title: keeping the host would
        # prefix every title and break matching on the part the user typed.
        host, _, title = rest.partition(" ")
        if not host:
            continue
        found.append({"hwnd": int(wid, 16), "pid": int(pid), "title": title,
                      "x": int(x), "y": int(y), "w": int(w), "h": int(h)})
    return found


def format_id(hwnd):
    """X11 ids are conventionally hexadecimal; printing them in decimal would not
    match anything else the window manager shows."""
    return "0x%08x" % hwnd


def parse_id(spec):
    """Accept 0x-prefixed hex, or a plain decimal number.

    A bare hexadecimal string is read as decimal on purpose: "1234" from a user
    means the window whose id is 1234, and silently reading it as 0x1234 would
    resolve to a different window.
    """
    return int(spec, 16) if spec.lower().startswith("0x") else int(spec)


def resolve_window(spec):
    wins = list_windows()
    if spec is None:
        return None
    spec = str(spec)
    if spec.lower().startswith("0x") or spec.isdigit():
        want = parse_id(spec)
        for w in wins:
            if w["hwnd"] == want:
                return w
        raise LookupError("no window with id %s (run: cu windows)" % spec)
    hits = [w for w in wins if spec.lower() in w["title"].lower()]
    if len(hits) == 1:
        return hits[0]
    if not hits:
        raise LookupError("no window matches %r (run: cu windows)" % spec)
    listing = "\n".join("  %s  %s" % (format_id(h["hwnd"]), h["title"]) for h in hits)
    raise LookupError("window %r is ambiguous, use an id:\n%s" % (spec, listing))


def foreground():
    wins = list_windows()
    res = _run(["xdotool", "getactivewindow"])
    try:
        active = int(res.stdout.strip())
    except ValueError:
        return wins[0] if wins else {"hwnd": 0, "pid": 0, "title": "(none)",
                                     "x": 0, "y": 0, "w": 0, "h": 0}
    for w in wins:
        if w["hwnd"] == active:
            return w
    return {"hwnd": active, "pid": 0, "title": "(unknown)", "x": 0, "y": 0, "w": 0, "h": 0}


def force_foreground(hwnd):
    if not _have("wmctrl"):
        raise RuntimeError("wmctrl not installed (apt install wmctrl)")
    _run(["wmctrl", "-i", "-a", format_id(int(hwnd))])
    return foreground()["hwnd"] == int(hwnd)


# xwininfo -tree prints one line per child:
#   <indent>0xID "name": ("class" "Class")  WxH+X+Y  +ABSX+ABSY
# The name is quoted and the geometry sits after the parenthesised class, so a
# split on "(" picks up the id instead of the name.
_CHILD = re.compile(
    r'^(?P<indent>[ ]*)(?P<id>0x[0-9a-fA-F]+)\s+"(?P<name>[^"]*)":\s*'
    # A negative coordinate prints as "+-10": a sign, then the signed value. Without
    # the optional inner sign the whole line fails to match and the child vanishes.
    r'\((?P<cls>[^)]*)\)\s+(?P<w>\d+)x(?P<h>\d+)'
    r'(?P<x>[+-]-?\d+)(?P<y>[+-]-?\d+)'
)


def tree(spec, depth=5, limit=150):
    """X11 child windows of one window: structure, not a widget tree.

    There is no X11 equivalent of UIA or AX. Toolkits expose one over AT-SPI,
    which needs pyatspi and a bus connection; rather than ship an unverified
    dependency for it, this reports window structure and says so.
    """
    if not _have("xwininfo"):
        raise RuntimeError("xwininfo not installed (apt install x11-utils)")
    win = foreground() if spec is None else resolve_window(spec)
    if _session_type() == "wayland":
        raise RuntimeError("xwininfo cannot inspect windows under Wayland")
    res = _run(["xwininfo", "-tree", "-id", str(win["hwnd"])])
    if res.returncode != 0:
        raise RuntimeError("xwininfo failed: %s" % res.stderr.strip())
    entries = []
    for line in res.stdout.splitlines():
        m = _CHILD.match(line)
        if m:
            entries.append((len(m.group("indent")), m))
    # Depth comes from indentation, which is the only nesting xwininfo encodes.
    indents = sorted({indent for indent, _ in entries})
    lines = []
    for indent, m in entries[:limit]:
        level = indents.index(indent) + 1
        if level > depth:
            continue
        name = m.group("name")
        if len(name) > 60:
            name = name[:57] + "..."
        # "+-10" means -10; normalising it keeps the line identical in shape to the
        # Windows and macOS backends, so a centre can be computed from it blind.
        def coord(field):
            return int(m.group(field).replace("+-", "-"))
        lines.append('%sPane "%s" @%d,%d %dx%d'
                     % ("  " * level, name, coord("x"), coord("y"),
                        int(m.group("w")), int(m.group("h"))))
    header = 'window "%s" id=%s pid=%s' % (win["title"], format_id(win["hwnd"]), win["pid"])
    lines.append("note: X11 window tree, not a widget tree")
    if len(entries) > limit:
        lines.append("!! truncated at %d entries; narrow with --depth" % limit)
    return header, lines


def type_text(text):
    if _session_type() == "wayland":
        if not _have("ydotool"):
            raise RuntimeError("Wayland needs ydotool for typing")
        res = _run(["ydotool", "type", "--", text])
        return len(text) * 2 if res.returncode == 0 else 0
    if not _have("xdotool"):
        raise RuntimeError("xdotool not installed (apt install xdotool)")
    res = _run(["xdotool", "type", "--clearmodifiers", "--", text])
    # xdotool reports success without a per-character count; 2 events per char is
    # what the caller verifies against.
    return len(text) * 2 if res.returncode == 0 else 0


def send_keys(combo):
    if _session_type() == "wayland":
        if not _have("ydotool"):
            raise RuntimeError("Wayland needs ydotool for keys")
        res = _run(["ydotool", "key", _ydotool_combo(combo)])
        return (len(combo.split("+")) * 2) if res.returncode == 0 else 0
    if not _have("xdotool"):
        raise RuntimeError("xdotool not installed (apt install xdotool)")
    res = _run(["xdotool", "key", "--clearmodifiers", combo])
    return (len(combo.split("+")) * 2) if res.returncode == 0 else 0


def _ydotool_combo(combo):
    names = {"ctrl": "ctrl", "alt": "alt", "shift": "shift", "win": "meta"}
    parts = [names.get(p, p) for p in combo.lower().split("+")]
    return ":".join("%s:1" % p for p in parts[:-1]) + " %s:1 %s:0" % (parts[-1], parts[-1])


def click(x, y, double=False, right=False):
    if _session_type() == "wayland":
        if not _have("ydotool"):
            raise RuntimeError("Wayland needs ydotool for clicks")
        button = "0xC1" if right else "0xC0"
        count = 2 if double else 1
        _run(["ydotool", "mousemove", "--absolute", "-x", str(int(x)), "-y", str(int(y))])
        for _ in range(count):
            _run(["ydotool", "click", button])
        return
    if not _have("xdotool"):
        raise RuntimeError("xdotool not installed (apt install xdotool)")
    args = ["xdotool", "mousemove", str(int(x)), str(int(y)), "click"]
    args.append("3" if right else "1")
    if double:
        args += ["click", "1"]
    _run(args)


def capture(path, window=None, scale=1.0):
    """Prefer `import` (ImageMagick) for its -resize, fall back to grim/gnome-screenshot."""
    if window is not None:
        target = ["-window", str(window["hwnd"])]
        src = (window["x"], window["y"], window["w"], window["h"])
    else:
        target = ["-window", "root"]
        src = (0, 0, 0, 0)
    if _have("import"):
        args = ["import"] + target
        if scale != 1:
            args += ["-resize", "%d%%" % int(scale * 100)]
        res = _run(args + [path])
    elif _have("grim"):
        args = ["grim"] + ([] if window is None else ["-g", "%d,%d %dx%d" % src])
        if scale != 1:
            args += ["-s", str(scale)]
        res = _run(args + [path])
    elif _have("gnome-screenshot"):
        res = _run(["gnome-screenshot", "-f", path])
    else:
        raise RuntimeError("no capture tool found (apt install imagemagick for `import`)")
    if res.returncode != 0:
        raise RuntimeError("capture failed: %s" % (res.stderr.strip() or res.returncode))
    w, h = _pixels(path)
    if src[2] == 0:
        src = (0, 0, w, h)
    return {"w": w, "h": h, "src": src, "scale": scale}


def _pixels(path):
    if _have("identify"):
        res = _run(["identify", "-format", "%w %h", path])
        bits = res.stdout.split()
        if len(bits) == 2:
            return int(bits[0]), int(bits[1])
    return 0, 0


def doctor():
    out = [("Linux backend is unverified by its author; treat results with suspicion", True)]
    kind = _session_type()
    out.append(("session type: %s" % kind, kind in ("x11", "wayland")))
    ok = True
    for tool, why in (("wmctrl", "window list and focus"),
                      ("xdotool", "input under X11"),
                      ("import", "capture (ImageMagick)")):
        found = _have(tool)
        out.append(("%s %s (%s)" % (tool, "found" if found else "MISSING", why), found))
        if not found and kind == "x11":
            ok = False
    try:
        wins = list_windows()
        out.append(("window list: %d entries" % len(wins), len(wins) > 0))
        ok = ok and bool(wins)
    except Exception as exc:  # noqa: BLE001
        out.append(("window list FAILED: %s" % exc, False))
        ok = False
    return out, ok
