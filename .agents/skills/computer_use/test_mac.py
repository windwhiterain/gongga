#!/usr/bin/env python3
"""Parse-level tests for the macOS backend, runnable on any OS.

`cumac.py` cannot be exercised for real without a Mac, but a good part of it is
its own logic: key-combination parsing and the AXValue unwrapping. Both are tested
here against stub modules, so what is asserted is the code in this repo and not
Apple's behaviour.

    python3 test_mac.py        # exits non-zero on failure
"""

import importlib.util
import os
import sys
import types

HERE = os.path.dirname(os.path.abspath(__file__))


class FakeQuartz(types.ModuleType):
    """Records posted events instead of sending them anywhere."""

    kCGHIDEventTap = 0
    kCGEventLeftMouseDown, kCGEventLeftMouseUp = 1, 2
    kCGEventRightMouseDown, kCGEventRightMouseUp = 3, 4
    kCGEventMouseMoved = 5
    kCGMouseButtonLeft, kCGMouseButtonRight = 0, 1
    kCGMouseEventClickState = 1

    def __init__(self):
        super().__init__("Quartz")
        self.events = []

    def CGEventCreateKeyboardEvent(self, _src, code, down):
        return ("key", code, down)

    def CGEventKeyboardSetUnicodeString(self, event, _length, text):
        self.events.append(("unicode", event[1], text))

    def CGEventPost(self, _tap, event):
        self.events.append(event)

    def CGEventCreateMouseEvent(self, _src, kind, point, button):
        return ("mouse", kind, point, button)

    def CGEventSetIntegerValueField(self, _event, _field, _value):
        return None

    def CGPointMake(self, x, y):
        return (x, y)


def load():
    spec = importlib.util.spec_from_file_location("cumac", os.path.join(HERE, "cumac.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    quartz = FakeQuartz()
    sys.modules["Quartz"] = quartz
    mod._AS = types.SimpleNamespace()  # skips the real pyobjc import in _libs()
    mod._Quartz = quartz.replace = None or quartz
    return mod, quartz


def check_key_parsing(mod, quartz):
    quartz.events.clear()
    # cmd+shift+s: three modifiers/keys down, then the same three up, in reverse.
    sent = mod.send_keys("cmd+shift+s")
    assert sent == 6, "expected 6 events, got %d" % sent
    downs = [e for e in quartz.events if e[0] == "key"]
    # cmd=55, shift=56, and "s" must be its real keycode (1), not ASCII 83.
    assert [e[1] for e in downs[:3]] == [55, 56, 1], downs[:3]
    assert [e[2] for e in downs[:3]] == [True, True, True], "modifiers must go down first"
    assert [e[2] for e in downs[3:]] == [False, False, False], "and come back up"
    assert [e[1] for e in downs[3:]] == [1, 56, 55], "released in reverse order"


def check_named_keys(mod, quartz):
    quartz.events.clear()
    mod.send_keys("enter")
    keys = [e for e in quartz.events if e[0] == "key"]
    assert [e[1] for e in keys] == [36, 36], keys
    # A named key that does not exist must be refused, not silently dropped.
    for bad in ("hyper+a", "cmd+nosuchkey", ""):
        try:
            mod.send_keys(bad)
        except ValueError:
            continue
        raise AssertionError("%r should have been refused" % bad)


def check_typing(mod, quartz):
    quartz.events.clear()
    text = "你好 ok"
    sent = mod.type_text(text)
    assert sent == len(text) * 2, "expected %d events, got %d" % (len(text) * 2, sent)
    typed = [e[2] for e in quartz.events if e[0] == "unicode"]
    expected = [c for c in text for _ in range(2)]
    assert typed == expected, "each char is set once per down and once per up: %r" % (typed,)


def check_value_unwrap(mod):
    # Both shapes pyobjc might use must produce a rectangle.
    point = types.SimpleNamespace(x=10, y=20)
    size = types.SimpleNamespace(width=100, height=50)
    box = types.SimpleNamespace(
        kAXValueCGPointType=1, kAXValueCGSizeType=2,
        AXValueGetValue=lambda value, _type, _out: (
            (True, point) if value == "pos" else (True, size)))
    mod._libs = lambda: (box, None)
    mod._attr = lambda el, name: "pos" if name == "AXPosition" else "size"
    assert mod._box(object()) == (10, 20, 100, 50), mod._box(object())

    # The bare-value shape.
    box2 = types.SimpleNamespace(
        kAXValueCGPointType=1, kAXValueCGSizeType=2,
        AXValueGetValue=lambda value, _type, _out: point if value == "pos" else size)
    mod._libs = lambda: (box2, None)
    assert mod._box(object()) == (10, 20, 100, 50), mod._box(object())

    # A failure must read as "no rectangle" rather than raising.
    box3 = types.SimpleNamespace(kAXValueCGPointType=1, kAXValueCGSizeType=2,
                                 AXValueGetValue=lambda *a: (False, None))
    mod._libs = lambda: (box3, None)
    assert mod._box(object()) is None


def check_verified_flag(mod):
    # The unverified status has to be machine-readable, for `doctor` to print.
    assert hasattr(mod, "VERIFIED") and "UNVERIFIED" in mod.VERIFIED, mod.VERIFIED


def main():
    mod, quartz = load()
    checks = [
        lambda: check_key_parsing(mod, quartz),
        lambda: check_named_keys(mod, quartz),
        lambda: check_typing(mod, quartz),
        lambda: check_value_unwrap(mod),
        lambda: check_verified_flag(mod),
    ]
    names = ["key_parsing", "named_keys", "typing", "value_unwrap", "verified_flag"]
    failed = 0
    for name, check in zip(names, checks):
        try:
            check()
            print("ok   %s" % name)
        except Exception as exc:  # noqa: BLE001
            failed += 1
            print("FAIL %s: %s: %s" % (name, type(exc).__name__, exc))
    print("%d/%d checks passed" % (len(checks) - failed, len(checks)))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
