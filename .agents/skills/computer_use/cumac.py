"""macOS backend: CGWindowList for windows, Accessibility for the UI tree, Quartz for input.

Status: NOT verified on a real Mac. Every call here follows documented Quartz and
Accessibility behaviour, but nothing in this file has been run, because the machine
this was written on is Windows. `doctor` reports it as unverified on purpose: a
capability table that claims more than it has measured is worse than none.

Requirements: `pyobjc-framework-Quartz` (already in cu.py's inline dependencies).
The UI tree additionally needs the calling terminal to be granted Accessibility
permission, and capture needs Screen Recording permission; both are one-time
System Settings prompts, and `doctor` names whichever is missing.
"""

import subprocess
import time

VERIFIED = "UNVERIFIED: written from documented Quartz/Accessibility APIs, never run on macOS"

_AS = None
_Quartz = None


def _libs():
    global _AS, _Quartz
    if _AS is None:
        import ApplicationServices as AS
        import Quartz
        _AS, _Quartz = AS, Quartz
    return _AS, _Quartz


def _run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True)


def list_windows():
    """Visible windows with their app names, via CGWindowListCopyWindowInfo."""
    AS, Quartz = _libs()
    opts = Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements
    info = Quartz.CGWindowListCopyWindowInfo(opts, Quartz.kCGNullWindowID) or []
    found = []
    for w in info:
        bounds = w.get("kCGWindowBounds") or {}
        width = int(bounds.get("Width", 0) or 0)
        height = int(bounds.get("Height", 0) or 0)
        if width <= 0 or height <= 0:
            continue
        owner = w.get("kCGWindowOwnerName") or ""
        title = w.get("kCGWindowName") or owner
        found.append({
            "hwnd": int(w.get("kCGWindowNumber", 0)),
            "pid": int(w.get("kCGWindowOwnerPID", 0)),
            "title": "%s - %s" % (title, owner) if title != owner else str(title),
            "x": int(bounds.get("X", 0) or 0), "y": int(bounds.get("Y", 0) or 0),
            "w": width, "h": height,
        })
    return found


def resolve_window(spec):
    wins = list_windows()
    if spec is None:
        return None
    spec = str(spec)
    if spec.isdigit():
        for w in wins:
            if w["hwnd"] == int(spec):
                return w
        raise LookupError("no window with id %s (run: cu windows)" % spec)
    hits = [w for w in wins if spec.lower() in w["title"].lower()]
    if len(hits) == 1:
        return hits[0]
    if not hits:
        raise LookupError("no window matches %r (run: cu windows)" % spec)
    listing = "\n".join("  %d  %s" % (h["hwnd"], h["title"]) for h in hits)
    raise LookupError("window %r is ambiguous, use an id:\n%s" % (spec, listing))


def foreground():
    wins = list_windows()
    script = 'tell application "System Events" to get unix id of first process whose frontmost is true'
    res = _run(["osascript", "-e", script])
    try:
        pid = int(res.stdout.strip())
    except ValueError:
        return wins[0] if wins else {"hwnd": 0, "pid": 0, "title": "(none)",
                                     "x": 0, "y": 0, "w": 0, "h": 0}
    for w in wins:
        if w["pid"] == pid:
            return w
    return {"hwnd": 0, "pid": pid, "title": "(pid %d)" % pid, "x": 0, "y": 0, "w": 0, "h": 0}


def force_foreground(hwnd):
    """Raise the window's app, then verify the frontmost pid really changed.

    Activation goes through System Events rather than NSRunningApplication: that
    class lives in AppKit, not Quartz, so reaching it through the Quartz module
    would depend on a re-export that is not part of the documented surface.
    """
    win = resolve_window(str(hwnd))
    script = ('tell application "System Events" to set frontmost of '
              '(first process whose unix id is %d) to true' % win["pid"])
    _run(["osascript", "-e", script])
    time.sleep(0.2)
    return foreground()["pid"] == win["pid"]


# --- UI tree -----------------------------------------------------------------

_ROLES = {
    "AXButton": "Button", "AXTextField": "Edit", "AXTextArea": "Edit",
    "AXStaticText": "Text", "AXMenuItem": "MenuItem", "AXMenuBar": "MenuBar",
    "AXGroup": "Group", "AXWindow": "Window", "AXScrollArea": "Pane",
    "AXRadioButton": "RadioButton", "AXCheckBox": "CheckBox",
    "AXPopUpButton": "ComboBox", "AXTabGroup": "Tab", "AXRow": "ListItem",
    "AXList": "List", "AXOutline": "Tree", "AXSlider": "Slider",
    "AXToolbar": "ToolBar", "AXImage": "Image", "AXLink": "Hyperlink",
}


def _attr(el, name):
    AS, _ = _libs()
    err, value = AS.AXUIElementCopyAttributeValue(el, name, None)
    return value if err == 0 else None


def _value(fn, *call_args):
    """Unwrap pyobjc's (returned, out-value) pair, tolerating a bare value.

    Which shape AXValueGetValue takes is one of the things this file cannot know
    without a Mac to run on, so both shapes are accepted rather than one assumed.
    """
    result = fn(*call_args)
    if isinstance(result, tuple):
        ok, value = result[0], result[-1]
        return value if ok else None
    return result


def _box(el):
    """Screen rectangle of an element, read out of its AXValue attributes."""
    AS, _ = _libs()
    pos = _attr(el, "AXPosition")
    size = _attr(el, "AXSize")
    if pos is None or size is None:
        return None
    point = _value(AS.AXValueGetValue, pos, AS.kAXValueCGPointType, None)
    extent = _value(AS.AXValueGetValue, size, AS.kAXValueCGSizeType, None)
    if point is None or extent is None:
        return None
    try:
        return (int(point.x), int(point.y), int(extent.width), int(extent.height))
    except AttributeError:
        return None


def tree(spec, depth=5, limit=150):
    """Walk the Accessibility tree of one window. Needs Accessibility permission."""
    AS, _ = _libs()
    win = foreground() if spec is None else resolve_window(spec)
    app = AS.AXUIElementCreateApplication(win["pid"])
    lines = []
    state = {"n": 0}

    def visit(el, d):
        if d > depth or state["n"] >= limit:
            return
        children = _attr(el, "AXChildren") or []
        for child in children:
            if state["n"] >= limit:
                return
            role = _attr(child, "AXRole") or "?"
            ctype = _ROLES.get(str(role), str(role).replace("AX", "") or "Unknown")
            name = str(_attr(child, "AXTitle") or _attr(child, "AXDescription") or "")
            if len(name) > 60:
                name = name[:57] + "..."
            box = _box(child)
            where = "@?,? ?x?" if box is None else "@%d,%d %dx%d" % box
            if name or ctype in ("Button", "Edit", "MenuItem", "TabItem", "Hyperlink"):
                lines.append('%s%s "%s" %s' % ("  " * d, ctype, name, where))
                state["n"] += 1
            visit(child, d + 1)

    visit(app, 1)
    header = 'window "%s" id=%s pid=%s' % (win["title"], win["hwnd"], win["pid"])
    if state["n"] >= limit:
        lines.append("!! truncated at %d elements; narrow with --depth" % limit)
    return header, lines


# --- input -------------------------------------------------------------------


def type_text(text):
    import Quartz
    sent = 0
    for ch in text:
        for down in (True, False):
            event = Quartz.CGEventCreateKeyboardEvent(None, 0, down)
            Quartz.CGEventKeyboardSetUnicodeString(event, len(ch), ch)
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)
            sent += 1
    return sent


_VK = {"enter": 36, "tab": 48, "esc": 53, "space": 49, "backspace": 51,
       "delete": 117, "left": 123, "right": 124, "down": 125, "up": 126,
       "home": 115, "end": 119, "pageup": 116, "pagedown": 121}

# Letters need their real virtual keycode, not an ASCII value: a shortcut such as
# cmd+s is matched against the keycode, so posting keycode 0 with a unicode string
# types the character into a text field but does not fire the menu item.
_LETTER_VK = {
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8,
    "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17,
    "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45,
    "m": 46, "1": 18, "2": 19, "3": 20, "4": 21, "5": 23, "6": 22, "7": 26,
    "8": 28, "9": 25, "0": 29,
}
_MOD = {"cmd": 55, "ctrl": 59, "alt": 58, "shift": 56}


def send_keys(combo):
    import Quartz
    parts = [p for p in combo.lower().split("+") if p]
    if not parts:
        raise ValueError("empty key combination")
    mods, key = [], None
    for p in parts:
        if p in _MOD:
            mods.append(_MOD[p])
        elif p in _VK:
            key = _VK[p]
        elif len(p) == 1 and p in _LETTER_VK:
            key = _LETTER_VK[p]
        elif len(p) == 1:
            key = p  # unmapped character: falls back to the unicode path below
        else:
            raise ValueError("unknown key %r" % p)
    if key is None:
        raise ValueError("no key in %r" % combo)
    sent = 0
    for code in mods:
        Quartz.CGEventPost(Quartz.kCGHIDEventTap,
                           Quartz.CGEventCreateKeyboardEvent(None, code, True))
        sent += 1
    if isinstance(key, int):
        for down in (True, False):
            Quartz.CGEventPost(Quartz.kCGHIDEventTap,
                               Quartz.CGEventCreateKeyboardEvent(None, key, down))
            sent += 1
    else:
        for down in (True, False):
            event = Quartz.CGEventCreateKeyboardEvent(None, 0, down)
            Quartz.CGEventKeyboardSetUnicodeString(event, len(key), key)
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)
            sent += 1
    for code in reversed(mods):
        Quartz.CGEventPost(Quartz.kCGHIDEventTap,
                           Quartz.CGEventCreateKeyboardEvent(None, code, False))
        sent += 1
    return sent


def click(x, y, double=False, right=False):
    import Quartz
    point = Quartz.CGPointMake(float(x), float(y))
    button = Quartz.kCGMouseButtonRight if right else Quartz.kCGMouseButtonLeft
    down = Quartz.kCGEventRightMouseDown if right else Quartz.kCGEventLeftMouseDown
    up = Quartz.kCGEventRightMouseUp if right else Quartz.kCGEventLeftMouseUp
    Quartz.CGEventPost(Quartz.kCGHIDEventTap,
                       Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventMouseMoved, point, 0))
    presses = 2 if double else 1
    for n in range(presses):
        for kind in (down, up):
            event = Quartz.CGEventCreateMouseEvent(None, kind, point, button)
            Quartz.CGEventSetIntegerValueField(event, Quartz.kCGMouseEventClickState, n + 1)
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)


# --- capture -----------------------------------------------------------------


def capture(path, window=None, scale=1.0):
    """Capture with the built-in screencapture, then let `sips` do any scaling."""
    if window is not None:
        args = ["screencapture", "-x", "-o", "-l", str(window["hwnd"]), path]
        src = (window["x"], window["y"], window["w"], window["h"])
    else:
        args = ["screencapture", "-x", path]
        src = (0, 0, 0, 0)
    res = _run(args)
    if res.returncode != 0:
        raise RuntimeError("screencapture failed: %s" % (res.stderr.strip() or res.returncode))
    w, h = _pixels(path)
    if scale != 1 and w and h:
        _run(["sips", "-z", str(max(1, int(h * scale))), str(max(1, int(w * scale))), path])
        w, h = _pixels(path)
    if src[2] == 0:
        src = (0, 0, w, h)
    return {"w": w, "h": h, "src": src, "scale": scale}


def _pixels(path):
    res = _run(["sips", "-g", "pixelWidth", "-g", "pixelHeight", path])
    w = h = 0
    for line in res.stdout.splitlines():
        if "pixelWidth" in line:
            w = int(line.split(":")[1])
        if "pixelHeight" in line:
            h = int(line.split(":")[1])
    return w, h


def doctor():
    out = [("macOS backend is unverified by its author; treat results with suspicion", True)]
    ok = True
    try:
        _libs()
        out.append(("pyobjc Quartz + ApplicationServices import", True))
    except Exception as exc:  # noqa: BLE001
        out.append(("pyobjc missing (%s) -- uv adds it from the inline dependencies" % exc, False))
        return out, False
    wins = list_windows()
    out.append(("CGWindowList: %d windows" % len(wins), len(wins) > 0))
    ok = ok and bool(wins)
    res = _run(["screencapture", "-x", "/tmp/cu-doctor.png"])
    out.append(("screencapture %s" % ("ok" if res.returncode == 0 else "FAILED: " + res.stderr),
                res.returncode == 0))
    ok = ok and res.returncode == 0
    AS, _ = _libs()
    trusted = AS.AXIsProcessTrusted()
    out.append(("Accessibility permission %s (needed for tree and input)"
                % ("granted" if trusted else "NOT granted"), bool(trusted)))
    return out, ok
