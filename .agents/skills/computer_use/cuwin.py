"""Windows backend: window enumeration, UI tree, mouse, keyboard, capture.

Two things here are not stylistic choices but responses to measured failures:

* The UI tree never starts from the root element. ``RootElement.FindAll`` blocks for
  about 210 seconds from a Python COM client on this machine, because the UIA core
  waits on a top-level provider that never answers. The .NET wrapper PowerShell uses
  hides that with its own client-side provider, which is why the same call there takes
  0.3 seconds. Top-level windows therefore come from ``EnumWindows``, and UIA is only
  asked about one window at a time.
* ``INPUT`` is declared so ctypes lays out the union itself. Hand-written offsets are
  what breaks: on x64 the union belongs at byte 8, and getting that wrong makes
  ``SendInput`` report success while delivering nothing.
"""

import ctypes
import os
import struct
import time
import zlib
from ctypes import wintypes as wt

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

user32.SetProcessDPIAware()
dwmapi = ctypes.windll.dwmapi
DWMWA_CLOAKED = 14

# --- input -------------------------------------------------------------------

KEYUP = 0x0002
UNICODE = 0x0004


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [("dx", wt.LONG), ("dy", wt.LONG), ("mouseData", wt.DWORD),
                ("dwFlags", wt.DWORD), ("time", wt.DWORD),
                ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong))]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", wt.WORD), ("wScan", wt.WORD), ("dwFlags", wt.DWORD),
                ("time", wt.DWORD), ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong))]


class _INPUTUNION(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT)]


class INPUT(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _INPUTUNION)]


VK = {
    "ctrl": 0x11, "shift": 0x10, "alt": 0x12, "win": 0x5B,
    "enter": 0x0D, "tab": 0x09, "esc": 0x1B, "space": 0x20,
    "backspace": 0x08, "delete": 0x2E, "up": 0x26, "down": 0x28,
    "left": 0x25, "right": 0x27, "home": 0x24, "end": 0x23,
    "pageup": 0x21, "pagedown": 0x22, "insert": 0x2D,
    "printscreen": 0x2C, "capslock": 0x14,
}
for _n in range(1, 25):
    VK["f%d" % _n] = 0x6F + _n


def _send(inputs):
    """Send a list of INPUT records; returns how many the system accepted."""
    if not inputs:
        return 0
    arr = (INPUT * len(inputs))(*inputs)
    return user32.SendInput(len(inputs), arr, ctypes.sizeof(INPUT))


def type_text(text):
    """Type literal text as Unicode code units, so any script arrives intact."""
    events = []
    for ch in text:
        events.append(INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(0, ord(ch), UNICODE, 0, None))))
        events.append(INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(0, ord(ch), UNICODE | KEYUP, 0, None))))
    return _send(events)


def send_keys(combo):
    """Press a key combination such as ctrl+shift+s. Modifiers release in reverse."""
    parts = [p for p in combo.lower().split("+") if p]
    if not parts:
        raise ValueError("empty key combination")
    vks = []
    for p in parts:
        if p in VK:
            vks.append(VK[p])
        elif len(p) == 1:
            vks.append(ord(p.upper()))
        else:
            raise ValueError("unknown key %r; known: %s, f1-f24, or one letter"
                             % (p, " ".join(sorted(VK))))
    events = [INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(vk, 0, 0, 0, None))) for vk in vks]
    events += [INPUT(type=1, u=_INPUTUNION(ki=KEYBDINPUT(vk, 0, KEYUP, 0, None)))
               for vk in reversed(vks)]
    return _send(events)


def click(x, y, double=False, right=False):
    user32.SetCursorPos(int(x), int(y))
    time.sleep(0.04)
    down, up = (0x0010, 0x0020) if right else (0x0002, 0x0004)
    for _ in range(2 if double else 1):
        user32.mouse_event(down, 0, 0, 0, 0)
        user32.mouse_event(up, 0, 0, 0, 0)
        if double:
            time.sleep(0.06)


# --- windows -----------------------------------------------------------------


def list_windows():
    """Visible top-level windows with titles, via Win32 rather than UIA."""
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def visit(hwnd, _):
        if not user32.IsWindowVisible(hwnd):
            return True
        # UWP apps keep hidden windows around; DWM calls them cloaked. They are
        # visible to EnumWindows but nothing can be clicked in them.
        cloaked = wt.DWORD(0)
        if dwmapi.DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED,
                                        ctypes.byref(cloaked), ctypes.sizeof(cloaked)) == 0:
            if cloaked.value:
                return True
        n = user32.GetWindowTextLengthW(hwnd)
        if n == 0:
            return True
        buf = ctypes.create_unicode_buffer(n + 1)
        user32.GetWindowTextW(hwnd, buf, n + 1)
        rect = wt.RECT()
        if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
            return True
        pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        found.append({
            "hwnd": int(hwnd), "pid": pid.value, "title": buf.value,
            "x": rect.left, "y": rect.top,
            "w": rect.right - rect.left, "h": rect.bottom - rect.top,
        })
        return True

    user32.EnumWindows(visit, 0)
    return found


def resolve_window(spec):
    """Accept a hwnd or a title fragment; refuse to guess between matches."""
    wins = list_windows()
    if spec is None:
        return None
    spec = str(spec)
    if spec.isdigit():
        for w in wins:
            if str(w["hwnd"]) == spec:
                return w
        raise LookupError("no window with hwnd %s (run: cu windows)" % spec)
    hits = [w for w in wins if spec.lower() in w["title"].lower()]
    if len(hits) == 1:
        return hits[0]
    if not hits:
        raise LookupError("no window matches %r (run: cu windows)" % spec)
    listing = "\n".join("  %d  %s" % (h["hwnd"], h["title"]) for h in hits)
    raise LookupError("window %r is ambiguous, use a hwnd:\n%s" % (spec, listing))


def foreground():
    hwnd = user32.GetForegroundWindow()
    for w in list_windows():
        if w["hwnd"] == hwnd:
            return w
    return {"hwnd": int(hwnd), "title": "(untitled)", "pid": 0,
            "x": 0, "y": 0, "w": 0, "h": 0}


def force_foreground(hwnd):
    """Bring a window forward and confirm it worked.

    Windows refuses SetForegroundWindow from a process that is not already the
    foreground one, and fails silently, which would send later keystrokes to
    whatever the user is looking at. Sharing the foreground thread's input queue
    lifts that restriction; the return value still reports whether it really moved.
    """
    hwnd = int(hwnd)
    if user32.GetForegroundWindow() == hwnd:
        return True
    if user32.IsIconic(hwnd):
        user32.ShowWindowAsync(hwnd, 9)  # SW_RESTORE
    fg = user32.GetForegroundWindow()
    fg_thread = user32.GetWindowThreadProcessId(fg, None) if fg else 0
    mine = ctypes.windll.kernel32.GetCurrentThreadId()
    attached = bool(fg_thread and fg_thread != mine and user32.AttachThreadInput(mine, fg_thread, True))
    user32.BringWindowToTop(hwnd)
    user32.ShowWindowAsync(hwnd, 5)  # SW_SHOW
    user32.SetForegroundWindow(hwnd)
    if attached:
        user32.AttachThreadInput(mine, fg_thread, False)
    time.sleep(0.15)
    return user32.GetForegroundWindow() == hwnd


# --- UI tree -----------------------------------------------------------------

INTERESTING = frozenset((
    "Button", "Edit", "MenuItem", "TabItem", "ListItem", "CheckBox",
    "RadioButton", "ComboBox", "Document", "Hyperlink",
))

CONTROL_TYPES = {
    50000: "Button", 50001: "Calendar", 50002: "CheckBox", 50003: "ComboBox",
    50004: "Edit", 50005: "Hyperlink", 50006: "Image", 50007: "ListItem",
    50008: "List", 50009: "Menu", 50010: "MenuBar", 50011: "MenuItem",
    50012: "ProgressBar", 50013: "RadioButton", 50014: "ScrollBar",
    50015: "Slider", 50016: "Spinner", 50017: "StatusBar", 50018: "Tab",
    50019: "TabItem", 50020: "Text", 50021: "ToolBar", 50022: "ToolTip",
    50023: "Tree", 50024: "TreeItem", 50025: "Custom", 50026: "Group",
    50027: "Thumb", 50028: "DataGrid", 50029: "DataItem", 50030: "Document",
    50031: "SplitButton", 50032: "Window", 50033: "Pane", 50034: "Header",
    50035: "HeaderItem", 50036: "Table", 50037: "TitleBar", 50038: "Separator",
    50039: "SemanticZoom", 50040: "AppBar",
}


def _uia():
    """Create the UIA client once per process."""
    global _UIA_CLIENT
    try:
        return _UIA_CLIENT
    except NameError:
        pass
    import sys
    # UIA wants a multithreaded apartment; comtypes defaults to single-threaded.
    if "comtypes" not in sys.modules:
        sys.coinit_flags = 0
    from comtypes.client import CreateObject, GetModule
    mod = GetModule("UIAutomationCore.dll")
    _UIA_CLIENT = CreateObject(mod.CUIAutomation, interface=mod.IUIAutomation)
    globals()["_UIA_CLIENT"] = _UIA_CLIENT
    return _UIA_CLIENT


TREE_CHILDREN = 0x2


def tree(spec, depth=5, limit=150):
    """Walk the control view of one window. Returns (header, lines).

    Uses FindAll(Children) recursively rather than IUIAutomationTreeWalker: the
    typelib module comtypes generates exposes the walker as an untyped pointer
    without its methods, so walking through it fails with AttributeError.
    FindAll on a specific element is fast; only the root element is slow, and it
    is never asked.
    """
    if spec is None:
        win = foreground()
    else:
        win = resolve_window(spec)
    uia = _uia()
    root = uia.ElementFromHandle(win["hwnd"])
    everything = uia.CreateTrueCondition()
    lines = []
    state = {"n": 0}

    def visit(el, d):
        if d > depth or state["n"] >= limit:
            return
        children = el.FindAll(TREE_CHILDREN, everything)
        for i in range(children.Length):
            if state["n"] >= limit:
                return
            child = children.GetElement(i)
            ctype = CONTROL_TYPES.get(child.CurrentControlType, "Unknown")
            name = child.CurrentName or ""
            if len(name) > 60:
                name = name[:57] + "..."
            box = child.CurrentBoundingRectangle
            if box.right == 0 and box.bottom == 0:
                where = "@?,? ?x?"
            else:
                where = "@%d,%d %dx%d" % (box.left, box.top,
                                          box.right - box.left, box.bottom - box.top)
            if name or ctype in INTERESTING:
                lines.append('%s%s "%s" %s' % ("  " * d, ctype, name, where))
                state["n"] += 1
            visit(child, d + 1)

    visit(root, 1)
    header = "window %r hwnd=%d pid=%d" % (win["title"], win["hwnd"], win["pid"])
    if state["n"] >= limit:
        lines.append("!! truncated at %d elements; narrow with --depth" % limit)
    return header, lines


# --- capture -----------------------------------------------------------------

_MODE_HALFTONE = 4
SRCCOPY = 0x00CC0020


class _BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
                ("biPlanes", wt.WORD), ("biBitCount", wt.WORD),
                ("biCompression", wt.DWORD), ("biSizeImage", wt.DWORD),
                ("biXPelsPerMeter", wt.LONG), ("biYPelsPerMeter", wt.LONG),
                ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD)]


def _png(path, width, height, bgra):
    """Write 32-bit BGRA pixels as an RGB PNG, using only zlib."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        row = bgra[y * stride:(y + 1) * stride]
        px = bytearray(width * 3)
        px[0::3] = row[2::4]
        px[1::3] = row[1::4]
        px[2::3] = row[0::4]
        raw.append(0)  # filter: none
        raw.extend(px)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    with open(path, "wb") as fh:
        fh.write(b"\x89PNG\r\n\x1a\n")
        fh.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
        fh.write(chunk(b"IDAT", zlib.compress(bytes(raw), 6)))
        fh.write(chunk(b"IEND", b""))
    return len(raw)


def capture(path, window=None, scale=1.0):
    """Copy the desktop or one window into a PNG, scaled by StretchBlt."""
    if window is None:
        src = (0, 0, user32.GetSystemMetrics(0), user32.GetSystemMetrics(1))
    else:
        if user32.IsIconic(window["hwnd"]):
            raise RuntimeError("window %d is minimized; run: cu focus %d"
                               % (window["hwnd"], window["hwnd"]))
        src = (window["x"], window["y"], window["w"], window["h"])
    if src[2] <= 0 or src[3] <= 0:
        raise RuntimeError("window %d has no visible area" % (window["hwnd"] if window else 0))
    tw = max(1, int(src[2] * scale))
    th = max(1, int(src[3] * scale))
    screen_dc = user32.GetDC(0)
    mem_dc = gdi32.CreateCompatibleDC(screen_dc)
    bmp = gdi32.CreateCompatibleBitmap(screen_dc, tw, th)
    gdi32.SelectObject(mem_dc, bmp)
    gdi32.SetStretchBltMode(mem_dc, _MODE_HALFTONE)
    gdi32.StretchBlt(mem_dc, 0, 0, tw, th, screen_dc, src[0], src[1], src[2], src[3], SRCCOPY)
    info = _BITMAPINFOHEADER(ctypes.sizeof(_BITMAPINFOHEADER), tw, -th, 1, 32, 0, 0, 0, 0, 0, 0)
    buf = ctypes.create_string_buffer(tw * th * 4)
    gdi32.GetDIBits(mem_dc, bmp, 0, th, buf, ctypes.byref(info), 0)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem_dc)
    user32.ReleaseDC(0, screen_dc)
    _png(path, tw, th, buf.raw)
    return {"w": tw, "h": th, "src": src, "scale": scale}


# --- self check ---------------------------------------------------------------


def doctor():
    """Probe the pieces this backend depends on; never claim more than measured."""
    out = []
    ok = True
    sw, sh = user32.GetSystemMetrics(0), user32.GetSystemMetrics(1)
    out.append(("desktop %dx%d (DPI aware, physical pixels)" % (sw, sh), sw > 0))
    ok = ok and sw > 0

    found = list_windows()
    out.append(("EnumWindows: %d visible titled windows" % len(found), len(found) > 0))

    size = ctypes.sizeof(INPUT)
    want = 40 if ctypes.sizeof(ctypes.c_void_p) == 8 else 28
    out.append(("INPUT is %d bytes (expected %d)" % (size, want), size == want))
    ok = ok and size == want

    try:
        import comtypes  # noqa: F401
        _uia()
        probe = foreground()
        _, lines = tree(probe["hwnd"], depth=2, limit=5)
        out.append(("UIA via comtypes: %d elements under %r" % (len(lines), probe["title"][:30]),
                    True))
    except Exception as exc:  # noqa: BLE001 - doctor reports, it does not raise
        out.append(("UIA via comtypes: FAILED (%s)" % exc, False))
        ok = False

    import tempfile
    tmp = os.path.join(tempfile.gettempdir(), "cu-doctor.png")
    try:
        info = capture(tmp, scale=0.1)
        out.append(("capture: %dx%d written" % (info["w"], info["h"]), True))
    except Exception as exc:  # noqa: BLE001
        out.append(("capture: FAILED (%s)" % exc, False))
        ok = False
    return out, ok
