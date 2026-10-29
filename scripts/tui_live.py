#!/usr/bin/env python3
r"""Watch the real gongga TUI render, from a pseudo-terminal, as text frames.

An agent cannot look at a window, so this harness gives it the bytes instead:
it starts the fake endpoint, a server whose home lives inside this worktree, and
`gongga-tui.exe` attached to a ConPTY; it types into that terminal; it decodes every
byte the client writes into a terminal grid; and it emits one text frame per
repaint with timing -- so streaming smoothness, screen jumps, flicker and
repaint cost can all be judged from a file.

    python scripts/tui_live.py --scenario single-stream
    python scripts/tui_live.py --scenario two-sessions --attrs
    python scripts/tui_live.py --scenario event-ui-toggles --cols 120 --rows 34
    python scripts/tui_live.py --script my.script
    python scripts/tui_live.py --list-scenarios

Everything it starts it kills, on every exit path including errors and Ctrl+C,
and everything it writes lands under `.dev/` (gitignored) apart from this file.

## What it prints

Per frame, into the frames file (default `.dev/live/<stamp>/frames.txt`):

    === frame 41  t=+1932ms  118 cells changed of 100x30  cur=27:3 (hidden)  in:812  erase:0/6  modes
    <grid rows, trailing blanks trimmed>
    -- attrs
    r0   20xK@C 80x.            (only with --attrs; one run-length line per row)

Cell codes: a letter for the foreground (`W` white, `k` dark gray, `C` cyan, `.`
terminal default, `T` truecolour), then `b`/`i`/`d`/`v` for bold/italic/dim/
inverse, then `@<letter>` for a background. Reasoning is `ki` (dark gray italic),
the answer `W`, a tool call `Y`, tool output `b`, the header bar `K@C`.

Then a summary: frames, frames per second over the busiest one-second window,
the largest bytes-arrived-to-frame-emitted gap, blank frames, collapse frames
(one showing under a quarter of what the frame before it showed), how many
frames erased the page, and row-loss frames. Those are the flicker numbers.

## Script line protocol

`<command> <argument text>`; the argument text is verbatim, so a message may
contain spaces. `#` starts a comment.

    wait <ms>                sleep, keeping frames flowing
    keys <text>              type, one character per --type-delay-ms
    burst <text>             type as fast as the pipe takes it
    enter                    CR
    key <name>               enter|esc|tab|backspace|delete|up|down|left|right|
                             home|end|pgup|pgdn|f1..f4
    alt <text>               Alt+<text>: ESC then the characters
    paste <text>             bracketed paste (ESC[200~ ... ESC[201~), one burst
    send-message <text>      burst + enter, i.e. prose to the agent
    resize <cols> <rows>     ResizePseudoConsole
    wheel up|down [n]        SGR wheel events, n clicks
    click <col> <row>        SGR press and release at that cell
    sleep-until-frame [n] [ms]   wait for n more frames (default 1), giving up after ms
    settle [timeout_ms]      wait until the client stops writing
    expect <text> [ms]       wait until the grid contains <text>
    gone <text> [ms]         wait until the grid no longer contains <text>
    label <text>             write a marker into the frame file

Escapes understood inside argument text: `\n`, `\t`, `\e`, `\\`. Anything else,
Chinese included, is literal.

## Scenarios

By name, from `scripts/tui_live_scenarios.py`: `single-stream`, `two-sessions`,
`event-ui-toggles`.

## The fake endpoint

`--fake plain` runs `scripts/fake_llm.py` as it is. `--fake stream` (what the
shipped scenarios ask for) runs a generated wrapper under `.dev/` that imports
that same file and keeps its handler, its event shapes, its usage measurement
and its request dump; it replaces only the one function that builds the event
list, so a single answer becomes many reasoning and output deltas with a delay
between them. That is the traffic this harness exists to watch render.

## Flags

    --scenario NAME | --script FILE     what to drive
    --fake plain|stream                 endpoint stand-in (default stream)
    --tool NAME --tool-args JSON        the tool the first request calls
    --cols 100 --rows 30                terminal size
    --quiet-ms 20                       bytes stop this long -> one frame
    --type-delay-ms 40                  human-ish typing
    --attrs                             append per-row attribute codes
    --frames-out PATH                   frames file
    --home PATH                         server home, default .dev/live-home
    --keep-home                         reuse the home instead of wiping it
    --python PATH                       interpreter for the fake endpoint
    --server-exe/--client-exe           binaries (default target/debug)
    --replay FILE                       decode a captured raw.bin, no stack
"""

import argparse
import base64
import codecs
import ctypes
import importlib.util
import json
import os
import queue
import shutil
import socket
import subprocess
import sys
import threading
import time
import unicodedata
from ctypes import (
    POINTER,
    Structure,
    byref,
    c_int,
    c_short,
    c_ssize_t,
    c_ubyte,
    c_uint,
    c_uint32,
    c_ushort,
    c_void_p,
    c_wchar_p,
    create_string_buffer,
)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEV = os.path.join(ROOT, ".dev")

# ---------------------------------------------------------------------------
# ConPTY
# ---------------------------------------------------------------------------

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

LPDWORD = POINTER(c_uint32)
SIZE_T = c_ssize_t
ULONG_PTR = c_ssize_t

EXTENDED_STARTUPINFO_PRESENT = 0x00080000
CREATE_UNICODE_ENVIRONMENT = 0x00000400
PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = 0x00020016
ZERO_MEMORY = 0x00000040
ERROR_BROKEN_PIPE = 109
ERROR_PIPE_NOT_CONNECTED = 233
ERROR_INVALID_HANDLE = 6
ERROR_OPERATION_ABORTED = 1224
STILL_ACTIVE = 259
PIPE_BUF_BYTES = 1 << 16
READ_BUF_BYTES = 1 << 16


class COORD(Structure):
    _fields_ = [("X", c_short), ("Y", c_short)]


class SECURITY_ATTRIBUTES(Structure):
    _fields_ = [
        ("nLength", c_uint32),
        ("lpSecurityDescriptor", c_void_p),
        ("bInheritHandle", c_int),
    ]


class STARTUPINFOW(Structure):
    _fields_ = [
        ("cb", c_uint32),
        ("lpReserved", c_wchar_p),
        ("lpDesktop", c_wchar_p),
        ("lpTitle", c_wchar_p),
        ("dwX", c_int),
        ("dwY", c_int),
        ("dwXSize", c_uint32),
        ("dwYSize", c_uint32),
        ("dwXCountChars", c_uint32),
        ("dwYCountChars", c_uint32),
        ("dwFillAttribute", c_uint32),
        ("dwFlags", c_uint32),
        ("wShowWindow", c_ushort),
        ("cbReserved2", c_ushort),
        ("lpReserved2", POINTER(c_ubyte)),
        ("hStdInput", c_void_p),
        ("hStdOutput", c_void_p),
        ("hStdError", c_void_p),
    ]


class STARTUPINFOEXW(Structure):
    _fields_ = [("base", STARTUPINFOW), ("lpAttributeList", c_void_p)]


class PROCESS_INFORMATION(Structure):
    _fields_ = [
        ("hProcess", c_void_p),
        ("hThread", c_void_p),
        ("dwProcessId", c_uint32),
        ("dwThreadId", c_uint32),
    ]


kernel32.CreatePseudoConsole.argtypes = [COORD, c_void_p, c_void_p, c_uint32, POINTER(c_void_p)]
kernel32.CreatePseudoConsole.restype = c_int
kernel32.ResizePseudoConsole.argtypes = [c_void_p, COORD]
kernel32.ResizePseudoConsole.restype = c_int
kernel32.ClosePseudoConsole.argtypes = [c_void_p]
kernel32.CreatePipe.argtypes = [
    POINTER(c_void_p),
    POINTER(c_void_p),
    POINTER(SECURITY_ATTRIBUTES),
    c_uint32,
]
kernel32.CreatePipe.restype = c_int
kernel32.ReadFile.argtypes = [c_void_p, c_void_p, c_uint32, LPDWORD, c_void_p]
kernel32.ReadFile.restype = c_int
kernel32.WriteFile.argtypes = [c_void_p, c_void_p, c_uint32, LPDWORD, c_void_p]
kernel32.WriteFile.restype = c_int
kernel32.OpenThread.argtypes = [c_uint32, c_int, c_uint32]
kernel32.OpenThread.restype = c_void_p
kernel32.CancelSynchronousIo.argtypes = [c_void_p]
kernel32.CancelSynchronousIo.restype = c_int
kernel32.GetCurrentThreadId.restype = c_uint32
kernel32.CloseHandle.argtypes = [c_void_p]
kernel32.GetExitCodeProcess.argtypes = [c_void_p, LPDWORD]
kernel32.GetExitCodeProcess.restype = c_int
kernel32.TerminateProcess.argtypes = [c_void_p, c_uint]
kernel32.TerminateProcess.restype = c_int
kernel32.LocalAlloc.restype = c_void_p
kernel32.LocalFree.argtypes = [c_void_p]
kernel32.InitializeProcThreadAttributeList.argtypes = [c_void_p, c_uint32, c_uint32, POINTER(SIZE_T)]
kernel32.InitializeProcThreadAttributeList.restype = c_int
kernel32.UpdateProcThreadAttribute.argtypes = [
    c_void_p,
    c_uint32,
    ULONG_PTR,
    c_void_p,
    SIZE_T,
    c_void_p,
    POINTER(SIZE_T),
]
kernel32.UpdateProcThreadAttribute.restype = c_int
kernel32.DeleteProcThreadAttributeList.argtypes = [c_void_p]
kernel32.CreateProcessW.argtypes = [
    c_wchar_p,
    c_wchar_p,
    c_void_p,
    c_void_p,
    c_int,
    c_uint32,
    c_void_p,
    c_wchar_p,
    POINTER(STARTUPINFOEXW),
    POINTER(PROCESS_INFORMATION),
]
kernel32.CreateProcessW.restype = c_int


class ConPtyError(RuntimeError):
    pass


class EndOfStream(Exception):
    """The pseudo-console's output pipe closed: the client is gone."""


def _last_error():
    return ctypes.WinError(ctypes.get_last_error())


def _check(result, what):
    if not result:
        raise ConPtyError("%s failed: %s" % (what, _last_error()))


class ConPty:
    """A pseudo-console plus the process attached to it.

    Input goes to a pipe the console reads; output comes from a pipe the console
    writes. Anonymous pipes have no real overlapped mode -- a `ReadFile` with an
    OVERLAPPED there blocks like any other read -- so the reading happens on a
    thread of its own that stamps each arrival the moment it returns, and the
    frame loop waits on that queue instead of on the pipe. `CancelSynchronousIo`
    is how the reader is released on the way out.

    The process under the console is not the client but a small bridge written
    into the run directory. CreateProcessW honours `PROC_THREAD_ATTRIBUTE_
    PSEUDOCONSOLE` and gives the bridge the pseudo-console as its *console*, but
    its standard handles are inherited from whoever launched this script -- a
    file or a pipe when an agent runs it -- and a client whose stdout is not a
    console handle refuses to enter the alternate screen at all. The bridge,
    being inside the console already, opens `CONIN$`/`CONOUT$` and hands those
    to the client as its standard handles.
    """

    def __init__(self, cols, rows, bridge_path, python, status_path):
        self.cols = cols
        self.rows = rows
        self.bridge_path = bridge_path
        self.python = python
        self.status_path = status_path
        self.pcon = None
        self.h_process = None
        self.h_thread = None
        self.pid = None
        self._write_lock = threading.Lock()
        self._reader_tid = None
        self._in_read = None
        self._in_write = None
        self._out_read = None
        self._out_write = None

        # Non-inheritable: a child of the bridge must not end up holding the
        # console's own pipe ends.
        attributes = SECURITY_ATTRIBUTES(ctypes.sizeof(SECURITY_ATTRIBUTES), None, 0)
        in_read, in_write = c_void_p(), c_void_p()
        out_read, out_write = c_void_p(), c_void_p()
        _check(kernel32.CreatePipe(byref(in_read), byref(in_write), byref(attributes), PIPE_BUF_BYTES), "CreatePipe(input)")
        _check(kernel32.CreatePipe(byref(out_read), byref(out_write), byref(attributes), PIPE_BUF_BYTES), "CreatePipe(output)")
        self._in_read, self._in_write = in_read.value, in_write.value
        self._out_read, self._out_write = out_read.value, out_write.value

        handle = c_void_p()
        result = kernel32.CreatePseudoConsole(COORD(cols, rows), self._in_read, self._out_write, 0, byref(handle))
        if result != 0:
            raise ConPtyError("CreatePseudoConsole failed: 0x%08x" % (result & 0xFFFFFFFF))
        self.pcon = handle.value
        # The console adopts the two ends it was handed rather than duplicating
        # them, so closing our copies here would leave the child without a
        # console and the output pipe broken. They go when the console does.
        # Resize on a worker thread: ResizePseudoConsole is documented to be
        # able to deadlock against the thread that created the console.
        self.resize(cols, rows)

    def spawn(self, exe, argv, cwd, env):
        command = " ".join(_quote(part) for part in [exe] + [str(argument) for argument in argv])
        payload = base64.b64encode(
            json.dumps({"command": command, "cwd": cwd, "status": self.status_path}).encode("utf-8")
        ).decode("ascii")
        bridge_line = "%s -B -u %s %s" % (_quote(self.python), _quote(self.bridge_path), payload)
        size = SIZE_T()
        kernel32.InitializeProcThreadAttributeList(None, 1, 0, byref(size))
        block = kernel32.LocalAlloc(ZERO_MEMORY, size)
        if not block:
            raise ConPtyError("no memory for the process attribute list")
        try:
            _check(kernel32.InitializeProcThreadAttributeList(block, 1, 0, byref(size)), "InitializeProcThreadAttributeList")
            # The ConPTY sample spells this attribute's value as the HPCON
            # itself, not a pointer to a variable holding it. A `byref` here is
            # accepted by UpdateProcThreadAttribute and then silently ignored by
            # CreateProcessW, which hands the child an ordinary console instead:
            # nothing ever arrives on the output pipe and the size is wrong.
            _check(
                kernel32.UpdateProcThreadAttribute(
                    block,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                    c_void_p(self.pcon),
                    ctypes.sizeof(c_void_p),
                    None,
                    None,
                ),
                "UpdateProcThreadAttribute(PSEUDOCONSOLE)",
            )
            info = STARTUPINFOEXW()
            info.base.cb = ctypes.sizeof(STARTUPINFOEXW)
            info.lpAttributeList = block
            process = PROCESS_INFORMATION()
            created = kernel32.CreateProcessW(
                None,
                bridge_line,
                None,
                None,
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                _environment_block(env),
                cwd,
                byref(info),
                byref(process),
            )
            if not created:
                raise ConPtyError("CreateProcessW(bridge) failed: %s" % _last_error())
            self.h_process, self.h_thread = process.hProcess, process.hThread
            self.pid = process.dwProcessId
        finally:
            kernel32.DeleteProcThreadAttributeList(block)
            kernel32.LocalFree(block)
        return command

    def write(self, data):
        if not data:
            return
        payload = create_string_buffer(data, len(data))
        written = c_uint32(0)
        with self._write_lock:
            if not kernel32.WriteFile(self._in_write, payload, c_uint32(len(data)), byref(written), None):
                error = ctypes.get_last_error()
                if error in (ERROR_BROKEN_PIPE, ERROR_INVALID_HANDLE, ERROR_PIPE_NOT_CONNECTED):
                    raise EndOfStream("the pseudo-console input pipe is gone")
                raise ConPtyError("WriteFile failed: %s" % ctypes.WinError(error))

    def read(self):
        """One blocking read, for the reader thread; returns (timestamp, bytes)."""
        kernel32.GetCurrentThreadId.restype = c_uint32
        self._reader_tid = kernel32.GetCurrentThreadId()
        buffer = create_string_buffer(READ_BUF_BYTES)
        available = c_uint32(0)
        if not kernel32.ReadFile(self._out_read, buffer, c_uint32(READ_BUF_BYTES), byref(available), None):
            error = ctypes.get_last_error()
            if error in (ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, ERROR_INVALID_HANDLE):
                raise EndOfStream("the pseudo-console output pipe closed")
            if error == ERROR_OPERATION_ABORTED:
                raise EndOfStream("the read was cancelled with the console")
            raise ConPtyError("ReadFile failed: %s" % ctypes.WinError(error))
        if available.value == 0:
            raise EndOfStream("the pseudo-console output ended")
        return time.perf_counter(), bytes(buffer[: available.value])

    def resize(self, cols, rows):
        outcome = []

        def call():
            outcome.append(kernel32.ResizePseudoConsole(self.pcon, COORD(cols, rows)))

        worker = threading.Thread(target=call, daemon=True)
        worker.start()
        worker.join(timeout=10.0)
        if not outcome:
            raise ConPtyError("ResizePseudoConsole never returned")
        if outcome[0] != 0:
            raise ConPtyError("ResizePseudoConsole failed: 0x%08x" % (outcome[0] & 0xFFFFFFFF))
        self.cols, self.rows = cols, rows

    def running(self):
        """Whether the client under the console is still alive.

        The bridge reports through the status file: `started <pid>` then
        `exited <code>`. Until it says otherwise the bridge is still waiting, so
        a missing or short file means "alive" and the run is not cut short.
        """
        if not self.h_process:
            return False
        state = c_uint32(0)
        if not kernel32.GetExitCodeProcess(self.h_process, byref(state)):
            return False
        if state.value != STILL_ACTIVE:
            return False
        try:
            with open(self.status_path, "r", encoding="utf-8") as handle:
                marks = handle.read().split()
        except OSError:
            return True
        return "exited" not in marks

    def exit_note(self):
        try:
            with open(self.status_path, "r", encoding="utf-8") as handle:
                return handle.read().strip()
        except OSError:
            return "(no status from the bridge)"

    def close(self):
        if not self.pcon:
            return
        pid = self.pid
        if self.h_process:
            kernel32.TerminateProcess(self.h_process, 0)
        if pid:
            # The client is the bridge's child, so the whole tree goes, not just
            # the process whose handle we hold.
            _kill_tree(pid)
        self._cancel_reader()
        kernel32.ClosePseudoConsole(self.pcon)
        self.pcon = None
        # The console has now released the ends it adopted; every handle in this
        # list is ours to free exactly once.
        for handle in (self.h_process, self.h_thread, self._in_read, self._in_write, self._out_read, self._out_write):
            if handle:
                kernel32.CloseHandle(handle)
        self.h_process = None
        self.h_thread = None

    def _cancel_reader(self):
        """Release the reader thread from its blocking ReadFile, if it is in one."""
        if not self._reader_tid:
            return
        THREAD_ALL_ACCESS = 0x1FFFFF
        handle = kernel32.OpenThread(THREAD_ALL_ACCESS, 0, c_uint32(self._reader_tid))
        if handle:
            kernel32.CancelSynchronousIo(c_void_p(handle))
            kernel32.CloseHandle(c_void_p(handle))


def write_bridge(path):
    """Generate the in-console launcher.

    It is a file rather than `-c` code because the client it starts outlives any
    quoting the command line could carry, and because a bridge that fails should
    leave a log behind rather than an empty screen.
    """
    source = r'''"""Generated by scripts/tui_live.py: starts the TUI client inside a pseudo-console.

CreateProcessW gives this process the pseudo-console as its console, but the
standard handles it was launched with belong to whoever started the harness, and
a client whose stdout is not a console handle will not take over the screen. So
open the console devices and pass those on.
"""
import base64
import ctypes
import json
import sys

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_ALL = 0x00000007
OPEN_EXISTING = 3
STARTF_USESTDHANDLES = 0x00000100
CREATE_UNICODE_ENVIRONMENT = 0x00000400
INFINITE = 0xFFFFFFFF


class SECURITY_ATTRIBUTES(ctypes.Structure):
    _fields_ = [("nLength", ctypes.c_uint32), ("lpSecurityDescriptor", ctypes.c_void_p), ("bInheritHandle", ctypes.c_int)]


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [
        ("cb", ctypes.c_uint32),
        ("lpReserved", ctypes.c_wchar_p),
        ("lpDesktop", ctypes.c_wchar_p),
        ("lpTitle", ctypes.c_wchar_p),
        ("dwX", ctypes.c_int),
        ("dwY", ctypes.c_int),
        ("dwXSize", ctypes.c_uint32),
        ("dwYSize", ctypes.c_uint32),
        ("dwXCountChars", ctypes.c_uint32),
        ("dwYCountChars", ctypes.c_uint32),
        ("dwFillAttribute", ctypes.c_uint32),
        ("dwFlags", ctypes.c_uint32),
        ("wShowWindow", ctypes.c_ushort),
        ("cbReserved2", ctypes.c_ushort),
        ("lpReserved2", ctypes.c_void_p),
        ("hStdInput", ctypes.c_void_p),
        ("hStdOutput", ctypes.c_void_p),
        ("hStdError", ctypes.c_void_p),
    ]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("hProcess", ctypes.c_void_p),
        ("hThread", ctypes.c_void_p),
        ("dwProcessId", ctypes.c_uint32),
        ("dwThreadId", ctypes.c_uint32),
    ]


kernel32.CreateFileW.argtypes = [
    ctypes.c_wchar_p,
    ctypes.c_uint32,
    ctypes.c_uint32,
    ctypes.POINTER(SECURITY_ATTRIBUTES),
    ctypes.c_uint32,
    ctypes.c_uint32,
    ctypes.c_void_p,
]
kernel32.CreateFileW.restype = ctypes.c_void_p
kernel32.CreateProcessW.argtypes = [
    ctypes.c_wchar_p,
    ctypes.c_wchar_p,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_int,
    ctypes.c_uint32,
    ctypes.c_void_p,
    ctypes.c_wchar_p,
    ctypes.POINTER(STARTUPINFOW),
    ctypes.POINTER(PROCESS_INFORMATION),
]
kernel32.CreateProcessW.restype = ctypes.c_int
kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
kernel32.WaitForSingleObject.restype = ctypes.c_uint32
kernel32.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
kernel32.GetExitCodeProcess.restype = ctypes.c_int


def mark(status_path, line):
    try:
        with open(status_path, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")
    except OSError:
        pass


def main():
    plan = json.loads(base64.b64decode(sys.argv[1]).decode("utf-8"))
    status_path = plan["status"]
    inheritable = SECURITY_ATTRIBUTES(ctypes.sizeof(SECURITY_ATTRIBUTES), None, 1)
    std_in = kernel32.CreateFileW("CONIN$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_ALL, ctypes.byref(inheritable), OPEN_EXISTING, 0, None)
    std_out = kernel32.CreateFileW("CONOUT$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_ALL, ctypes.byref(inheritable), OPEN_EXISTING, 0, None)
    if not std_in or not std_out or std_in == ctypes.c_void_p(-1).value or std_out == ctypes.c_void_p(-1).value:
        mark(status_path, "bridge-error CreateFileW %r" % (ctypes.WinError(ctypes.get_last_error()),))
        return 2
    info = PROCESS_INFORMATION()
    startup = STARTUPINFOW()
    startup.cb = ctypes.sizeof(STARTUPINFOW)
    startup.dwFlags = STARTF_USESTDHANDLES
    startup.hStdInput = std_in
    startup.hStdOutput = std_out
    startup.hStdError = std_out
    created = kernel32.CreateProcessW(
        None,
        plan["command"],
        None,
        None,
        1,
        CREATE_UNICODE_ENVIRONMENT,
        None,
        plan["cwd"],
        ctypes.byref(startup),
        ctypes.byref(info),
    )
    if not created:
        mark(status_path, "bridge-error CreateProcessW %r" % (ctypes.WinError(ctypes.get_last_error()),))
        return 2
    mark(status_path, "started %d" % info.dwProcessId)
    kernel32.WaitForSingleObject(info.hProcess, INFINITE)
    code = ctypes.c_uint32(0)
    kernel32.GetExitCodeProcess(info.hProcess, ctypes.byref(code))
    mark(status_path, "exited %d" % code.value)
    return int(code.value)


if __name__ == "__main__":
    sys.exit(main())
'''
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(source)



def _quote(part):
    text = str(part)
    if text and not any(character in text for character in ' \t"'):
        return text
    escaped = ""
    index = 0
    while index < len(text):
        backslashes = 0
        while index < len(text) and text[index] == "\\":
            backslashes += 1
            index += 1
        if index < len(text) and text[index] == '"':
            escaped += "\\" * (backslashes * 2 + 1) + '"'
            index += 1
            continue
        escaped += "\\" * (backslashes * 2)
        if index < len(text):
            escaped += text[index]
            index += 1
    return '"%s"' % escaped


def _environment_block(env):
    return "\0".join("%s=%s" % (key, value) for key, value in sorted(env.items())) + "\0\0"


def _child_environment(extra):
    """This shell's environment minus anything that points at another harness.

    GONGGA_SERVER / GONGGA_KEY / GONGGA_HOME are exported into every tool call's
    shell, so inheriting them would aim the client at whatever server is running
    the agent that is running this script.
    """
    env = dict(os.environ)
    for name in [key for key in env if key.startswith("GONGGA_")]:
        del env[name]
    env["TERM"] = "xterm-256color"
    env.update(extra)
    return env


# ---------------------------------------------------------------------------
# terminal grid
# ---------------------------------------------------------------------------

FG_NAMES = {
    0: "K", 1: "R", 2: "G", 3: "Y", 4: "B", 5: "M", 6: "C", 7: "W",
    8: "k", 9: "r", 10: "g", 11: "y", 12: "b", 13: "m", 14: "c", 15: "w",
}
BOLD, ITALIC, DIM, WIDE, INVERSE = 1, 2, 4, 8, 16
BLANK_CELL = (" ", None, None, 0)
CONTINUATION_CELL = ("\x00", None, None, WIDE)
CSI_PARAMETER = set("0123456789:;")
CSI_PRIVATE = set("<=>?")
CSI_INTERMEDIATE = set(" !\"#$%&'()*+,-./")
CSI_FINAL = {chr(code) for code in range(0x40, 0x7F)}


def display_width(character):
    if character == "\x00":
        return 0
    if unicodedata.combining(character):
        return 0
    return 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1


class Terminal:
    """The grid crossterm and ratatui are painting, as far as a frame needs it.

    The alternate screen, absolute and relative movement, erase in page and
    line, insert/delete line and character, the scrolling region, autowrap with
    its deferred flag, double-width cells, and the SGR set the client uses. Mode
    sets whose traffic is a request rather than a drawing -- mouse reports,
    bracketed paste, device status -- are recorded and swallowed.
    """

    def __init__(self, cols, rows):
        self.cols = cols
        self.rows = rows
        self.grid = [[BLANK_CELL] * cols for _ in range(rows)]
        self.alt = None
        self.alt_cursor = (0, 0, None, None, 0)
        self.cursor_row = 0
        self.cursor_col = 0
        self.cursor_visible = True
        self.autowrap = True
        self.pending_wrap = False
        self.modes = {}
        self.saved_cursor = (0, 0)
        self.region_top = 0
        self.region_bottom = rows - 1
        self.fg = None
        self.bg = None
        self.attrs = 0
        self.erase_page_ops = 0
        self.erase_line_ops = 0
        self.unknown_sequences = {}
        self.requests = 0
        self._state = "ground"
        self._collect = ""
        self._private = ""
        self._intermediate = ""
        self._string_body = ""
        self._string_escape = False

    # -- entry --------------------------------------------------------------

    def feed(self, text):
        for character in text:
            self._step(character)

    def _step(self, character):
        if self._state == "escape":
            self._escape(character)
            return
        if self._state == "string":
            self._string(character)
            return
        if self._state == "csi":
            self._csi(character)
            return
        if character == "\x1b":
            self._state = "escape"
            return
        if character in ("\x18", "\x1a"):
            self._state = "ground"
            return
        self._control_or_graphic(character)

    def _control_or_graphic(self, character):
        if character == "\r":
            self.cursor_col = 0
            self.pending_wrap = False
        elif character in ("\n", "\x0b", "\x0c"):
            self._line_feed()
        elif character == "\b":
            self.cursor_col = max(0, self.cursor_col - 1)
            self.pending_wrap = False
        elif character == "\t":
            self.cursor_col = min(self.cols - 1, (self.cursor_col // 8 + 1) * 8)
        elif character == "\x07":
            pass
        elif character >= " ":
            self._put(character)

    def _put(self, character):
        width = display_width(character)
        if width == 0:
            index = self.cursor_col - 1
            if index >= 0 and self.grid[self.cursor_row][index][0] not in (" ", "\x00"):
                cell = self.grid[self.cursor_row][index]
                self.grid[self.cursor_row][index] = (cell[0] + character, cell[1], cell[2], cell[3])
            return
        if self.pending_wrap and self.autowrap:
            self.cursor_col = 0
            self._advance_row()
        self.pending_wrap = False
        if width == 2 and self.cursor_col + 2 > self.cols:
            if self.autowrap and self.cursor_col + 1 >= self.cols:
                self.cursor_col = 0
                self._advance_row()
            else:
                width = 1
        self.grid[self.cursor_row][self.cursor_col] = (character, self.fg, self.bg, self.attrs)
        if width == 2 and self.cursor_col + 1 < self.cols:
            self.grid[self.cursor_row][self.cursor_col + 1] = CONTINUATION_CELL
        self.cursor_col += width
        if self.cursor_col >= self.cols:
            self.cursor_col = self.cols - 1
            self.pending_wrap = self.autowrap

    def _line_feed(self):
        if self.cursor_row == self.region_bottom:
            self._scroll_down()
        elif self.cursor_row < self.rows - 1:
            self.cursor_row += 1

    def _advance_row(self):
        self._line_feed()

    def _scroll_down(self):
        """Content moves up inside the region: the top row leaves, a blank comes in."""
        del self.grid[self.region_top]
        self.grid.insert(self.region_bottom, [BLANK_CELL] * self.cols)

    def _scroll_up(self):
        del self.grid[self.region_bottom]
        self.grid.insert(self.region_top, [BLANK_CELL] * self.cols)

    # -- escape and strings -------------------------------------------------

    def _escape(self, character):
        self._state = "ground"
        if character == "[":
            self._state = "csi"
            self._collect = ""
            self._private = ""
            self._intermediate = ""
            return
        if character in "]PX^_":
            self._state = "string"
            self._string_body = ""
            self._string_escape = False
            return
        if character == "7":
            self.saved_cursor = (self.cursor_row, self.cursor_col)
        elif character == "8":
            self.cursor_row, self.cursor_col = self.saved_cursor
            self.pending_wrap = False
        elif character == "M":
            if self.cursor_row == self.region_top:
                self._scroll_up()
            else:
                self.cursor_row -= 1
        elif character == "D":
            self._line_feed()
        elif character == "E":
            self.cursor_col = 0
            self._line_feed()
        elif character == "c":
            self.__init__(self.cols, self.rows)
        elif character not in "()#&":
            self._note_unknown("ESC " + character)

    def _string(self, character):
        # OSC (window titles, hyperlinks) and DCS payloads say nothing about a
        # cell; they end at BEL or at ST, which is ESC followed by a backslash.
        if self._string_escape:
            self._string_escape = False
            if character == "\\":
                self._state = "ground"
                return
            self._string_body += "\x1b" + character
            return
        if character == "\x07":
            self._state = "ground"
            return
        if character == "\x1b":
            self._string_escape = True
            return
        self._string_body += character

    def _csi(self, character):
        if character in CSI_PARAMETER:
            self._collect += character
        elif character in CSI_PRIVATE:
            self._private += character
        elif character in CSI_INTERMEDIATE:
            self._intermediate += character
        elif character in CSI_FINAL:
            self._dispatch(final=character)
            self._state = "ground"
        elif character == "\x1b":
            self._state = "escape"
        else:
            self._note_unknown("CSI ?")
            self._state = "ground"

    def _note_unknown(self, label):
        self.unknown_sequences[label] = self.unknown_sequences.get(label, 0) + 1

    def _parameters(self, default):
        if not self._collect:
            return [default]
        values = []
        for group in self._collect.replace(":", ";").split(";"):
            values.append(int(group) if group.isdigit() else default)
        return values

    def _dispatch(self, final):
        private = self._private
        if final in "hl" and private:
            self._mode(private, self._parameters(0), final == "h")
            return
        if private in ("<", ">", "="):
            self.requests += 1
            return
        if final == "H" or final == "f":
            values = self._parameters(1)
            row = values[0] - 1
            col = (values[1] if len(values) > 1 else 1) - 1
            self._move_to(row, col)
        elif final == "d":
            self._move_to(self._parameters(1)[0] - 1, self.cursor_col)
        elif final == "G" or final == "`":
            self._move_to(self.cursor_row, self._parameters(1)[0] - 1)
        elif final == "A":
            self._move_to(self.cursor_row - self._parameters(1)[0], self.cursor_col)
        elif final == "B":
            self._move_to(self.cursor_row + self._parameters(1)[0], self.cursor_col)
        elif final == "C":
            self._move_to(self.cursor_row, self.cursor_col + self._parameters(1)[0])
        elif final == "D":
            self._move_to(self.cursor_row, self.cursor_col - self._parameters(1)[0])
        elif final == "E":
            self._move_to(self.cursor_row + self._parameters(1)[0], 0)
        elif final == "F":
            self._move_to(self.cursor_row - self._parameters(1)[0], 0)
        elif final == "J":
            self._erase_page(self._parameters(0)[0])
        elif final == "K":
            self._erase_line(self._parameters(0)[0])
        elif final == "L":
            self._insert_lines(self._parameters(1)[0])
        elif final == "M":
            self._delete_lines(self._parameters(1)[0])
        elif final == "S":
            # crossterm's ScrollUp: the buffer moves up n rows inside the region,
            # which is `insert_before`'s way of making room below the prompt.
            self._scroll_region_up(self._parameters(1)[0])
        elif final == "T":
            self._scroll_region_down(self._parameters(1)[0])
        elif final == "P":
            self._delete_characters(self._parameters(1)[0])
        elif final == "@":
            self._insert_characters(self._parameters(1)[0])
        elif final == "X":
            count = self._parameters(1)[0]
            for col in range(self.cursor_col, min(self.cols, self.cursor_col + count)):
                self.grid[self.cursor_row][col] = BLANK_CELL
        elif final == "r":
            values = self._parameters(1)
            top = values[0] - 1
            bottom = (values[1] if len(values) > 1 else self.rows) - 1
            self.region_top = max(0, min(top, self.rows - 1))
            self.region_bottom = max(self.region_top, min(bottom, self.rows - 1))
        elif final == "s":
            self.saved_cursor = (self.cursor_row, self.cursor_col)
        elif final == "u":
            self.cursor_row, self.cursor_col = self.saved_cursor
            self.pending_wrap = False
        elif final == "m":
            self._select_graphic_rendition()
        elif final in "nc":
            self.requests += 1
        else:
            self._note_unknown("CSI %s%s" % (private, final))

    def _move_to(self, row, col):
        self.cursor_row = min(max(row, 0), self.rows - 1)
        self.cursor_col = min(max(col, 0), self.cols - 1)
        self.pending_wrap = False

    def _insert_lines(self, count):
        blank = [BLANK_CELL] * self.cols
        for _ in range(count):
            self.grid.insert(min(self.cursor_row, self.region_bottom), list(blank))
            if len(self.grid) > self.rows:
                del self.grid[self.region_bottom + 1]

    def _delete_lines(self, count):
        blank = [BLANK_CELL] * self.cols
        for _ in range(count):
            if self.cursor_row <= self.region_bottom:
                del self.grid[self.cursor_row]
            self.grid.insert(self.region_bottom, list(blank))

    def _scroll_region_up(self, count):
        blank = [BLANK_CELL] * self.cols
        for _ in range(count):
            del self.grid[self.region_top]
            self.grid.insert(self.region_bottom, list(blank))

    def _scroll_region_down(self, count):
        blank = [BLANK_CELL] * self.cols
        for _ in range(count):
            del self.grid[self.region_bottom]
            self.grid.insert(self.region_top, list(blank))

    def _delete_characters(self, count):
        row = self.grid[self.cursor_row]
        for _ in range(count):
            if self.cursor_col < len(row):
                del row[self.cursor_col]
            row.append(BLANK_CELL)

    def _insert_characters(self, count):
        row = self.grid[self.cursor_row]
        for _ in range(count):
            row.insert(self.cursor_col, BLANK_CELL)
        del row[self.cols:]

    def _erase_page(self, mode):
        self.erase_page_ops += 1
        if mode in (2, 3):
            self.grid = [[BLANK_CELL] * self.cols for _ in range(self.rows)]
        elif mode == 0:
            for col in range(self.cursor_col, self.cols):
                self.grid[self.cursor_row][col] = BLANK_CELL
            for row in range(self.cursor_row + 1, self.rows):
                self.grid[row] = [BLANK_CELL] * self.cols
        elif mode == 1:
            for row in range(0, self.cursor_row):
                self.grid[row] = [BLANK_CELL] * self.cols
            for col in range(0, min(self.cursor_col + 1, self.cols)):
                self.grid[self.cursor_row][col] = BLANK_CELL

    def _erase_line(self, mode):
        self.erase_line_ops += 1
        row = self.grid[self.cursor_row]
        if mode == 0:
            for col in range(self.cursor_col, self.cols):
                row[col] = BLANK_CELL
        elif mode == 1:
            for col in range(0, min(self.cursor_col + 1, self.cols)):
                row[col] = BLANK_CELL
        else:
            self.grid[self.cursor_row] = [BLANK_CELL] * self.cols

    def _mode(self, private, parameters, on):
        for parameter in parameters:
            name = private + str(parameter)
            self.modes[name] = on
            if private == "?" and parameter in (47, 1047, 1049):
                self._alternate_screen(on)
            elif private == "?" and parameter == 25:
                self.cursor_visible = on
            elif parameter == 7:
                self.autowrap = on

    def _alternate_screen(self, on):
        if on:
            self.alt = [list(row) for row in self.grid]
            self.alt_cursor = (self.cursor_row, self.cursor_col, self.fg, self.bg, self.attrs)
            self.grid = [[BLANK_CELL] * self.cols for _ in range(self.rows)]
            self._move_to(0, 0)
        elif self.alt is not None:
            self.grid = self.alt
            self.alt = None
            row, col, fg, bg, attrs = self.alt_cursor
            self.fg, self.bg, self.attrs = fg, bg, attrs
            self._move_to(row, col)

    # -- rendition ----------------------------------------------------------

    def _select_graphic_rendition(self):
        parameters = self._parameters(0)
        index = 0
        while index < len(parameters):
            code = parameters[index]
            if code == 0:
                self.fg, self.bg, self.attrs = None, None, 0
            elif code == 1:
                self.attrs |= BOLD
            elif code == 2:
                self.attrs |= DIM
            elif code == 3:
                self.attrs |= ITALIC
            elif code == 7:
                self.attrs |= INVERSE
            elif code == 22:
                self.attrs &= ~(BOLD | DIM)
            elif code == 23:
                self.attrs &= ~ITALIC
            elif code == 27:
                self.attrs &= ~INVERSE
            elif 30 <= code <= 37:
                self.fg = code - 30
            elif code == 39:
                self.fg = None
            elif 90 <= code <= 97:
                self.fg = code - 90 + 8
            elif 40 <= code <= 47:
                self.bg = code - 40
            elif code == 49:
                self.bg = None
            elif 100 <= code <= 107:
                self.bg = code - 100 + 8
            elif code in (38, 48):
                colour, consumed = _extended_colour(parameters, index)
                if code == 38:
                    self.fg = colour
                else:
                    self.bg = colour
                index += consumed
                continue
            elif code in (58, 65):
                _, consumed = _extended_colour(parameters, index)
                index += consumed
                continue
            index += 1

    # -- readings -----------------------------------------------------------

    def rows_text(self):
        lines = []
        for row in self.grid:
            chunks = []
            for cell in row:
                character = cell[0]
                if character == "\x00":
                    continue
                chunks.append(character)
            lines.append("".join(chunks).rstrip())
        return lines

    def snapshot(self):
        return tuple(tuple(row) for row in self.grid)

    def resize(self, cols, rows):
        grid = [[BLANK_CELL] * cols for _ in range(rows)]
        for row in range(min(rows, self.rows)):
            for col in range(min(cols, self.cols)):
                grid[row][col] = self.grid[row][col]
        self.cols, self.rows = cols, rows
        self.grid = grid
        self.region_top = 0
        self.region_bottom = rows - 1
        self._move_to(self.cursor_row, self.cursor_col)

    def filled_cells(self):
        return sum(
            1
            for row in self.grid
            for cell in row
            if cell != BLANK_CELL and cell != CONTINUATION_CELL
        )

    def filled_rows(self):
        return sum(
            1
            for row in self.grid
            if any(cell != BLANK_CELL and cell != CONTINUATION_CELL for cell in row)
        )

    def attribute_runs(self):
        lines = []
        for row in self.grid:
            runs = []
            code = None
            count = 0
            for cell in row:
                current = cell_code(cell)
                if current == code:
                    count += 1
                    continue
                if code is not None:
                    runs.append("%dx%s" % (count, code))
                code, count = current, 1
            if code is not None:
                runs.append("%dx%s" % (count, code))
            lines.append(" ".join(runs))
        return lines


def cell_code(cell):
    character, fg, bg, attrs = cell
    if attrs & WIDE:
        return "cont"
    if fg is None:
        symbol = "."
    elif isinstance(fg, int):
        symbol = FG_NAMES.get(fg, "?")
    else:
        symbol = "T"
    if attrs & INVERSE:
        symbol += "v"
    if attrs & BOLD:
        symbol += "b"
    if attrs & ITALIC:
        symbol += "i"
    if attrs & DIM:
        symbol += "d"
    if bg is not None:
        symbol += "@" + (FG_NAMES.get(bg, "T") if isinstance(bg, int) else "T")
    if character == " " and fg is None and bg is None and not attrs:
        return "."
    return symbol


def _extended_colour(parameters, index):
    mode = parameters[index + 1] if index + 1 < len(parameters) else 0
    if mode == 5:
        value = parameters[index + 2] if index + 2 < len(parameters) else 0
        return value, 3
    if mode == 2:
        red = parameters[index + 2] if index + 2 < len(parameters) else 0
        green = parameters[index + 3] if index + 3 < len(parameters) else 0
        blue = parameters[index + 4] if index + 4 < len(parameters) else 0
        return _quantise(red, green, blue), 5
    return None, 1


def _quantise(red, green, blue):
    lightness = (max(red, green, blue) + min(red, green, blue)) / 2.0
    if lightness > 192:
        return 15
    if lightness > 96:
        return 8
    return 0


# ---------------------------------------------------------------------------
# frames
# ---------------------------------------------------------------------------

class FrameWriter:
    def __init__(self, path, attrs, root):
        self.stream = open(path, "w", encoding="utf-8", newline="\n")
        self.attrs = attrs
        self.root = root
        self.index = 0
        self.last_emit = root
        self.records = []

    def note(self, text):
        self.stream.write("\n--- note t=+%.0fms %s\n" % ((time.perf_counter() - self.root) * 1000.0, text))
        self.stream.flush()

    def emit(self, terminal, previous, first_byte, last_byte, count):
        self.index += 1
        now = time.perf_counter()
        self.last_emit = now
        snapshot = terminal.snapshot()
        if previous is None or len(previous) != terminal.rows or len(previous[0]) != terminal.cols:
            changed = terminal.filled_cells()
        else:
            changed = sum(
                1
                for row in range(terminal.rows)
                for col in range(terminal.cols)
                if previous[row][col] != snapshot[row][col]
            )
        filled = terminal.filled_cells()
        modes = _mode_summary(terminal)
        self.stream.write(
            "\n=== frame %d  t=+%.0fms  %d cells changed of %dx%d  cur=%d:%d%s  in:%d  erase:%d/%d  %s\n"
            % (
                self.index,
                (now - self.root) * 1000.0,
                changed,
                terminal.cols,
                terminal.rows,
                terminal.cursor_row + 1,
                terminal.cursor_col + 1,
                "" if terminal.cursor_visible else " hidden",
                count,
                terminal.erase_page_ops,
                terminal.erase_line_ops,
                modes,
            )
        )
        for line in terminal.rows_text():
            self.stream.write(line + "\n")
        if self.attrs:
            self.stream.write("-- attrs\n")
            for row, line in enumerate(terminal.attribute_runs()):
                self.stream.write("r%-3d %s\n" % (row, line))
        self.stream.flush()
        self.records.append(
            {
                "index": self.index,
                "at": now,
                "changed": changed,
                "filled": filled,
                "rows": terminal.filled_rows(),
                "bytes": count,
                "lag_ms": (now - last_byte) * 1000.0 if last_byte else 0.0,
                "span_ms": (last_byte - first_byte) * 1000.0 if first_byte and last_byte else 0.0,
                "erase_page": terminal.erase_page_ops,
                "erase_line": terminal.erase_line_ops,
            }
        )
        terminal.erase_page_ops = 0
        terminal.erase_line_ops = 0
        return snapshot

    def close(self):
        try:
            self.stream.close()
        except OSError:
            pass


def _mode_summary(terminal):
    if not terminal.modes:
        return "no-modes"
    return "modes " + ",".join(
        "%s%s" % (name, "" if on else "-") for name, on in sorted(terminal.modes.items(), key=lambda item: (len(item[0]), item[0]))
    )


def summarise(records, terminal, wall_seconds, frames_path, raw_path):
    lines = ["frames: %d" % len(records), "wall: %.2fs" % wall_seconds]
    lines.append("frames file: %s" % frames_path)
    lines.append("byte tape: %s" % raw_path)
    if not records:
        return lines
    stamps = [record["at"] for record in records]
    window = 1.0
    best = 0
    start = 0
    for stop in range(len(stamps)):
        while stamps[stop] - stamps[start] > window:
            start += 1
        best = max(best, stop - start + 1)
    total_bytes = sum(record["bytes"] for record in records)
    cells = terminal.cols * terminal.rows
    blank = sum(1 for record in records if record["filled"] == 0)
    # A flash is a repaint that shows far less than the frame before it: the
    # screen is nearly empty for the length of one frame. Judged against the
    # previous frame rather than the whole grid, because a short log on a tall
    # screen is not a flash.
    collapse = sum(
        1
        for previous, current in zip(records, records[1:])
        if previous["filled"] > 0 and current["filled"] < previous["filled"] // 4
    )
    erased_then_thin = sum(
        1 for record in records if record["erase_page"] and previous_filled_drop(records, record)
    )
    row_loss = sum(
        1
        for previous, current in zip(records, records[1:])
        if current["rows"] + 3 <= previous["rows"]
    )
    lines.append("frames/sec over the busiest 1s window: %.1f" % (best / window))
    lines.append("largest bytes-arrived -> frame-emitted gap: %.1fms" % max(record["lag_ms"] for record in records))
    lines.append("longest byte span inside one frame: %.1fms" % max(record["span_ms"] for record in records))
    lines.append("bytes: %d total, %.0f per frame, %d in the biggest frame" % (total_bytes, total_bytes / len(records), max(record["bytes"] for record in records)))
    lines.append("largest single-frame repaint: %d cells of %d" % (max(record["changed"] for record in records), cells))
    lines.append("blank frames (nothing on screen): %d" % blank)
    lines.append("collapse frames (under a quarter of the frame before it): %d" % collapse)
    lines.append("frames that erased the page (ESC[2J): %d, of which the screen came back thin: %d" % (sum(1 for record in records if record["erase_page"]), erased_then_thin))
    lines.append("row-loss frames (3+ rows gone in one repaint): %d" % row_loss)
    return lines


def previous_filled_drop(records, record):
    """Whether this frame shows much less than the frame before it."""
    position = records.index(record)
    return position > 0 and record["filled"] < records[position - 1]["filled"] // 4


# ---------------------------------------------------------------------------
# input encoding
# ---------------------------------------------------------------------------

KEY_SEQUENCES = {
    "enter": b"\r",
    "esc": b"\x1b",
    "escape": b"\x1b",
    "tab": b"\t",
    "backspace": b"\x7f",
    "delete": b"\x1b[3~",
    "up": b"\x1b[A",
    "down": b"\x1b[B",
    "right": b"\x1b[C",
    "left": b"\x1b[D",
    "home": b"\x1b[H",
    "end": b"\x1b[F",
    "pgup": b"\x1b[5~",
    "pgdn": b"\x1b[6~",
    "f1": b"\x1bOP",
    "f2": b"\x1bOQ",
    "f3": b"\x1bOR",
    "f4": b"\x1bOS",
}


def decode_text(text):
    out = []
    index = 0
    while index < len(text):
        if text[index] == "\\" and index + 1 < len(text):
            replacement = {"n": "\n", "t": "\t", "e": "\x1b", "\\": "\\"}.get(text[index + 1])
            if replacement is not None:
                out.append(replacement)
                index += 2
                continue
        out.append(text[index])
        index += 1
    return "".join(out)


def mouse_sequence(button, col, row, press=True):
    return ("\x1b[<%d;%d;%d%s" % (button, col, row, "M" if press else "m")).encode("utf-8")


# ---------------------------------------------------------------------------
# driver
# ---------------------------------------------------------------------------

class Driver:
    """Runs the script against the live terminal, on its own thread, so that
    waiting for the UI never stops the frame loop."""

    def __init__(self, conpty, terminal, grid_lock, writer, condition, options):
        self.conpty = conpty
        self.terminal = terminal
        self.lock = grid_lock
        self.writer = writer
        self.condition = condition
        self.options = options
        self.error = None
        self.done = threading.Event()
        self.stop = threading.Event()

    def run(self, script):
        try:
            self.execute(script)
        except BaseException as error:
            self.error = error
        finally:
            self.done.set()
            with self.condition:
                self.condition.notify_all()

    def execute(self, script):
        for number, raw in enumerate(script, start=1):
            if self.stop.is_set():
                return
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            command, _, rest = line.partition(" ")
            try:
                self.perform(command, rest.strip(), number)
            except Exception as error:
                raise RuntimeError("script line %d (%s %s): %s" % (number, command, rest, error))

    def perform(self, command, argument, number):
        if command in ("wait", "sleep"):
            self.wait_ms(_number(argument, 0))
        elif command == "keys":
            self.type_text(decode_text(argument), self.options.type_delay_ms)
        elif command == "burst":
            self.type_text(decode_text(argument), 2)
        elif command == "paste":
            self.send(("\x1b[200~" + decode_text(argument) + "\x1b[201~").encode("utf-8"))
        elif command == "enter":
            self.send(b"\r")
        elif command == "key":
            sequence = KEY_SEQUENCES.get(argument.lower())
            if sequence is None:
                raise RuntimeError("unknown key name %r" % argument)
            self.send(sequence)
        elif command == "alt":
            self.send(b"\x1b" + decode_text(argument).encode("utf-8"))
            self.wait_ms(40)
        elif command == "send-message":
            self.type_text(decode_text(argument), 2)
            self.send(b"\r")
        elif command == "resize":
            parts = argument.split()
            if len(parts) != 2:
                raise RuntimeError("resize wants <cols> <rows>")
            self.resize(int(parts[0]), int(parts[1]))
        elif command == "wheel":
            parts = argument.split()
            self.wheel(parts[0] if parts else "up", int(parts[1]) if len(parts) > 1 else 1)
        elif command == "click":
            parts = argument.split()
            if len(parts) != 2:
                raise RuntimeError("click wants <col> <row>")
            self.click(int(parts[0]), int(parts[1]))
        elif command == "sleep-until-frame":
            parts = argument.split()
            how_many = int(parts[0]) if parts else 1
            limit = int(parts[1]) if len(parts) > 1 else None
            self.await_frames(how_many, limit)
        elif command == "settle":
            self.settle(_number(argument, 15000))
        elif command == "expect":
            self.expect(argument, True)
        elif command == "gone":
            self.expect(argument, False)
        elif command in ("label", "note"):
            self.writer.note(argument)
            print("[driver +%.1fs] %s" % (time.perf_counter() - self.writer.root, argument), flush=True)
        else:
            raise RuntimeError("unknown script command %r" % command)

    def send(self, data):
        self.conpty.write(data)

    def wait_ms(self, milliseconds):
        deadline = time.perf_counter() + milliseconds / 1000.0
        while not self.stop.is_set() and time.perf_counter() < deadline:
            time.sleep(min(0.02, max(0.0, deadline - time.perf_counter())))

    def type_text(self, text, delay_ms):
        for character in text:
            if self.stop.is_set():
                return
            self.send(character.encode("utf-8"))
            if delay_ms:
                self.wait_ms(delay_ms)

    def resize(self, cols, rows):
        self.conpty.resize(cols, rows)
        with self.lock:
            self.terminal.resize(cols, rows)
        self.writer.note("resized to %dx%d" % (cols, rows))

    def wheel(self, direction, count):
        button = 64 if direction.startswith("u") else 65
        col = max(1, self.terminal.cols // 2)
        row = max(1, min(self.terminal.rows - 1, self.terminal.rows - 6))
        for _ in range(count):
            self.send(mouse_sequence(button, col, row))
            self.wait_ms(25)

    def click(self, col, row):
        self.send(mouse_sequence(0, col, row))
        self.wait_ms(25)
        self.send(mouse_sequence(0, col, row, press=False))

    def await_frames(self, how_many, limit_ms=None):
        """Wait for the client to repaint, and say out loud when it did not.

        A key that changes nothing visible (a wheel at the newest row, say) is
        worth knowing about, so a timeout here is a finding rather than a hang.
        """
        timeout_ms = limit_ms if limit_ms is not None else self.options.settle_timeout_ms
        deadline = time.perf_counter() + timeout_ms / 1000.0
        with self.condition:
            target = self.writer.index + how_many
            while self.writer.index < target and not self.stop.is_set() and time.perf_counter() < deadline:
                self.condition.wait(0.1)
            arrived = self.writer.index >= target
        if not arrived:
            self.writer.note("no further frame within %dms (asked for %d)" % (timeout_ms, how_many))
        return arrived

    def settle(self, timeout_ms):
        quiet = max(self.options.quiet_ms * 12, 400) / 1000.0
        deadline = time.perf_counter() + timeout_ms / 1000.0
        while not self.stop.is_set() and time.perf_counter() < deadline:
            if time.perf_counter() - self.writer.last_emit >= quiet:
                return
            time.sleep(0.05)

    def expect(self, argument, want):
        needle, _, limit = argument.rpartition(" ")
        if limit.strip().isdigit():
            timeout_ms = int(limit)
        else:
            needle, timeout_ms = argument, 5000
        deadline = time.perf_counter() + timeout_ms / 1000.0
        while not self.stop.is_set() and time.perf_counter() < deadline:
            with self.lock:
                shown = "\n".join(self.terminal.rows_text())
            if (needle in shown) is want:
                return
            self.wait_ms(30)
        raise RuntimeError(
            "timed out after %dms waiting for %s%r on screen"
            % (timeout_ms, "" if want else "the absence of ", needle)
        )


def _number(text, default):
    try:
        return int(float(text))
    except ValueError:
        return default


def load_script(path):
    with open(path, "r", encoding="utf-8") as handle:
        return [line.rstrip("\n") for line in handle]


# ---------------------------------------------------------------------------
# the stack
# ---------------------------------------------------------------------------

def free_port(kind):
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM if kind == "udp" else socket.SOCK_STREAM)
    try:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]
    finally:
        probe.close()


def wait_for_tcp(port, timeout_seconds, processes):
    deadline = time.perf_counter() + timeout_seconds
    while time.perf_counter() < deadline:
        for process in processes:
            if process.poll() is not None:
                return False
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def wait_for_text(path, needle, timeout_seconds, processes):
    deadline = time.perf_counter() + timeout_seconds
    while time.perf_counter() < deadline:
        for process in processes:
            if process.poll() is not None:
                return False
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                if needle in handle.read():
                    return True
        except OSError:
            pass
        time.sleep(0.1)
    return False


def write_streaming_fake(path, fake_llm_path, chunks, delay_ms, reasoning, answer):
    """Generate the wrapper that widens scripts/fake_llm.py's stream.

    Nothing in the repo's file is changed or copied: it is imported, its handler,
    its dump and its usage measurement are used as they are, and only `events` is
    replaced -- with a *generator*, because the handler writes as it iterates. A
    list built with sleeps inside the constructor would still fire every delta
    into the socket in one burst, and there would be no stream to watch.
    """
    script = f'''"""Generated by scripts/tui_live.py: scripts/fake_llm.py, with a real stream."""
import importlib.util
import json
import sys
import threading
import time
from http.server import ThreadingHTTPServer

spec = importlib.util.spec_from_file_location("fake_llm", {fake_llm_path!r})
fake_llm = importlib.util.module_from_spec(spec)
sys.modules["fake_llm"] = fake_llm
spec.loader.exec_module(fake_llm)

original = fake_llm.events
CHUNKS = {chunks!r}
DELAY = {delay_ms!r} / 1000.0
REASONING = json.loads({reasoning!r})
ANSWER = json.loads({answer!r})


def split(text, parts):
    if not text:
        return [""]
    width = max(1, len(text) // max(1, parts))
    return [text[index:index + width] for index in range(0, len(text), width)] or [""]


def events(with_tool, with_web, with_image, usage=(0, 0)):
    """The same event shapes as the plain fake, one delta per DELAY seconds."""
    seq = iter(range(1, 100000))

    def ev(kind, **payload):
        return {{"event": kind, "data": {{**payload, "sequence_number": next(seq)}}}}

    def pace():
        # A small drift: identical gaps from the client's point of view are what
        # a provider does not do, and the frame file should show the difference.
        time.sleep(DELAY * (0.7 + (len(bin(next(seq))) % 7) / 10.0))

    reasoning_text = " ".join(REASONING)
    answer_text = " ".join(ANSWER)
    yield ev("response.created", response={{"id": "resp_live"}})
    yield ev("response.output_item.added", item={{"id": "rs_live", "type": "reasoning"}})
    for piece in split(reasoning_text, CHUNKS):
        yield ev("response.reasoning_text.delta", item_id="rs_live", delta=piece)
        pace()
    yield ev("response.output_item.done", item={{
        "id": "rs_live",
        "type": "reasoning",
        "summary": [{{"type": "summary_text", "text": reasoning_text}}],
    }})
    if with_web or with_image:
        for event in original(False, with_web, with_image, usage)[1:]:
            yield event
    if with_tool:
        call = {{"call_id": "call_live", "name": fake_llm.TOOL_NAME, "arguments": fake_llm.TOOL_ARGS}}
        yield ev("response.output_item.added", item={{"id": "fc_live", "type": "function_call", **call}})
        for piece in split(fake_llm.TOOL_ARGS, max(2, CHUNKS // 4)):
            yield ev("response.function_call_arguments.delta", item_id="fc_live", delta=piece)
            pace()
        yield ev("response.output_item.done", item={{"id": "fc_live", "type": "function_call", **call}})
    yield ev("response.output_item.added", item={{"id": "msg_live", "type": "message"}})
    for piece in split(answer_text, CHUNKS):
        yield ev("response.output_text.delta", item_id="msg_live", delta=piece)
        pace()
    yield ev("response.output_text.done", item_id="msg_live", text=answer_text)
    yield ev("response.output_item.done", item={{
        "id": "msg_live",
        "type": "message",
        "content": [{{"type": "output_text", "text": answer_text}}],
    }})
    yield ev("response.completed", response={{
        "id": "resp_live",
        "usage": {{
            "input_tokens": usage[0],
            "input_tokens_details": {{"cached_tokens": usage[1]}},
            "output_tokens": max(1, len(answer_text) // 4),
            "total_tokens": usage[0] + max(1, len(answer_text) // 4),
        }},
    }})


fake_llm.events = events

port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
server = ThreadingHTTPServer(("127.0.0.1", port), fake_llm.Handler)
print(f"fake llm streaming wrapper on http://127.0.0.1:{{port}}", flush=True)
threading.Thread(target=server.serve_forever, daemon=True).start()
threading.Event().wait()
'''
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(script)


def read_ship_pin():
    """The commit this build of the server installs, from `crates/server/ship.toml`.

    Read as text rather than parsed as TOML: the harness only has to name the
    object, and a full line match is the whole of that.
    """
    path = os.path.join(ROOT, "crates", "server", "ship.toml")
    try:
        with open(path, "r", encoding="utf-8") as handle:
            for line in handle:
                stripped = line.strip()
                if stripped.startswith("commit"):
                    _, _, rest = stripped.partition("=")
                    return rest.strip().strip('"').strip("'")
    except OSError:
        pass
    return None


def seed_ship_cache(home):
    """Put the pinned content in the home before the server asks for it.

    `ship::install` fetches the pin from its network URL and refuses to start
    when that fails and the home has never had the commit. The worktree carries
    the same content as the `ship/` submodule, so fetching from there leaves a
    cache the server accepts offline -- and the URL is a compiled-in pin, so a
    harness that never touches `crates/` has to do it this way.
    """
    source = os.path.join(ROOT, "ship")
    commit = read_ship_pin()
    if not os.path.isdir(source) or not commit:
        return "no local ship checkout; the server will need its network remote"
    cache = os.path.join(home, "ship.git")
    git = shutil.which("git")
    if not git:
        return "git is not on PATH; cannot seed the home's content cache"
    os.makedirs(home, exist_ok=True)
    # `git init --bare` will not create a missing parent, and a directory left by
    # an interrupted earlier run is not a repository: check for a working one
    # rather than for a name.
    if subprocess.run([git, "--git-dir", cache, "rev-parse", "--git-dir"], capture_output=True).returncode != 0:
        if os.path.isdir(cache):
            shutil.rmtree(cache, ignore_errors=True)
        os.makedirs(cache, exist_ok=True)
        init = subprocess.run([git, "--git-dir", cache, "init", "-q", "--bare"], capture_output=True)
        if init.returncode != 0:
            return "cannot create the content cache at %s: %s" % (cache, init.stderr.decode("utf-8", "replace").strip())
    subprocess.run(
        [git, "--git-dir", cache, "fetch", "-q", source, "+refs/heads/*:refs/heads/ship/*"],
        capture_output=True,
    )
    probe = subprocess.run(
        [git, "--git-dir", cache, "cat-file", "-e", "%s^{commit}" % commit],
        capture_output=True,
    )
    return "seeded %s with %s: %s" % (cache, commit[:8], "yes" if probe.returncode == 0 else "no")


class Stack:
    """The three processes, and the one place that knows how to stop them."""

    def __init__(self, options, run_dir, report=lambda stage: None):
        self.options = options
        self.run_dir = run_dir
        self.report = report
        self.fake = None
        self.server = None
        self.conpty = None
        self.fake_port = None
        self.server_port = None
        self.logs = []

    def _log(self, name):
        handle = open(os.path.join(self.run_dir, name), "w", encoding="utf-8", newline="\n")
        self.logs.append(handle)
        return handle

    def start_fake(self):
        port = free_port("tcp")
        environment = _child_environment({})
        if self.options.tool:
            environment["FAKE_LLM_TOOL"] = self.options.tool
        if self.options.tool_args:
            environment["FAKE_LLM_TOOL_ARGS"] = self.options.tool_args
        environment["FAKE_LLM_DUMP"] = os.path.join(self.run_dir, "requests.jsonl")
        if self.options.fake == "stream":
            target = os.path.join(self.run_dir, "fake_llm_stream.py")
            write_streaming_fake(
                target,
                os.path.join(ROOT, "scripts", "fake_llm.py"),
                max(1, self.options.stream_chunks),
                self.options.stream_delay_ms,
                self.options.reasoning_lines,
                self.options.answer_lines,
            )
            script = target
        else:
            script = os.path.join(ROOT, "scripts", "fake_llm.py")
        self.fake = subprocess.Popen(
            [self.options.python, script, str(port)],
            cwd=ROOT,
            env=environment,
            stdout=self._log("fake-llm.log"),
            stderr=subprocess.STDOUT,
        )
        if not wait_for_tcp(port, 15.0, [self.fake]):
            raise RuntimeError("the fake endpoint never listened on port %d" % port)
        self.fake_port = port
        return port

    def start_server(self, fake_port):
        os.makedirs(self.options.home, exist_ok=True)
        self.report("content cache: %s" % seed_ship_cache(self.options.home))
        port = free_port("udp")
        environment = _child_environment(
            {
                "GONGGA_BASE_URL": "http://127.0.0.1:%d/v1" % fake_port,
                "GONGGA_MODEL": self.options.model,
                "GONGGA_API_KEY": "fake-key-for-the-stand-in",
            }
        )
        argv = [
            self.options.server_exe,
            "--home",
            self.options.home,
            "--bind",
            "127.0.0.1:%d" % port,
            "--log",
            self.options.server_log,
        ]
        if self.options.bash:
            argv += ["--bash", self.options.bash]
        log_path = os.path.join(self.run_dir, "server.log")
        self.server = subprocess.Popen(
            argv, cwd=ROOT, env=environment, stdout=self._log("server.log"), stderr=subprocess.STDOUT
        )
        if not wait_for_text(log_path, "listening", 40.0, [self.server]):
            code = self.server.poll()
            raise RuntimeError(
                "the server did not report listening (exit %s); see %s" % (code, log_path)
            )
        self.server_port = port
        return port

    def start_client(self, cols, rows):
        environment = _child_environment(
            {
                "GONGGA_SERVER": "127.0.0.1:%d" % self.server_port,
                "GONGGA_HOME": self.options.home,
            }
        )
        bridge = os.path.join(self.run_dir, "pty_bridge.py")
        write_bridge(bridge)
        self.conpty = ConPty(cols, rows, bridge, self.options.python, os.path.join(self.run_dir, "client.status"))
        command = self.conpty.spawn(
            self.options.client_exe,
            ["--home", self.options.home, "--server", "127.0.0.1:%d" % self.server_port],
            ROOT,
            environment,
        )
        self.client_command = command
        return self.conpty

    def stop(self):
        if self.conpty:
            try:
                self.conpty.close()
            except Exception as error:
                print("warning: closing the pseudo-console: %s" % error, file=sys.stderr)
            self.conpty = None
        for name in ("server", "fake"):
            process = getattr(self, name)
            if process is None:
                continue
            _kill_tree(process.pid)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
        for handle in self.logs:
            try:
                handle.close()
            except OSError:
                pass
        self.logs = []


def _kill_tree(pid):
    # /T because the server's persistent bash shells are its children, and a
    # session must not leave anything running behind it.
    subprocess.run(
        ["taskkill", "/F", "/T", "/PID", str(pid)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


# ---------------------------------------------------------------------------
# run
# ---------------------------------------------------------------------------

def run_live(options, script):
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = os.path.join(DEV, "live", stamp)
    os.makedirs(run_dir, exist_ok=True)
    frames_path = options.frames_out or os.path.join(run_dir, "frames.txt")
    raw_path = os.path.join(run_dir, "raw.bin")

    def say(stage):
        print("[%6.2fs] %s" % (time.perf_counter() - root, stage), flush=True)

    stack = Stack(options, run_dir, say)
    terminal = Terminal(options.cols, options.rows)
    grid_lock = threading.Lock()
    condition = threading.Condition()
    root = time.perf_counter()
    writer = FrameWriter(frames_path, options.attrs, root)
    incoming = queue.Queue()
    stop = threading.Event()

    pending = bytearray()
    first_byte = None
    last_byte = None
    previous = None

    def flush():
        """Decode what arrived and hand it to the frame file as one frame."""
        nonlocal previous, pending, first_byte, last_byte
        if not pending:
            return
        terminal.feed(pending.decode("utf-8", "replace"))
        previous = writer.emit(terminal, previous, first_byte, last_byte, len(pending))
        with condition:
            condition.notify_all()
        pending = bytearray()
        first_byte = None
        last_byte = None

    reader_thread = None
    driver = Driver(conpty=None, terminal=terminal, grid_lock=grid_lock, writer=writer, condition=condition, options=options)
    driver_thread = None
    raw_stream = open(raw_path, "wb")
    try:
        say("fake endpoint: port %d" % stack.start_fake())
        say("server: 127.0.0.1:%d, home %s" % (stack.start_server(stack.fake_port), options.home))
        conpty = stack.start_client(options.cols, options.rows)
        say("client: bridge pid %d, %dx%d pseudo-console\n         %s" % (conpty.pid, options.cols, options.rows, stack.client_command))
        driver.conpty = conpty

        def reader():
            # One blocking read per arrival, stamped the instant it returns: the
            # queue's own timeout is what makes the frame loop non-blocking.
            while not stop.is_set():
                try:
                    when, data = conpty.read()
                except EndOfStream:
                    break
                except ConPtyError as error:
                    if not stop.is_set():
                        incoming.put(("error", error))
                    break
                incoming.put(("bytes", (when, data)))
            if not stop.is_set():
                incoming.put(("eof", time.perf_counter()))

        reader_thread = threading.Thread(target=reader, daemon=True)
        reader_thread.start()
        driver_thread = threading.Thread(target=driver.run, args=(script,), daemon=True)
        driver_thread.start()

        while True:
            try:
                kind, payload = incoming.get(timeout=options.quiet_ms / 1000.0)
            except queue.Empty:
                flush()
                if driver.done.is_set() and (incoming.empty() or not conpty.running()):
                    break
                continue
            if kind == "eof":
                flush()
                break
            if kind == "error":
                raise payload
            when, data = payload
            raw_stream.write(data)
            raw_stream.flush()
            if not pending:
                first_byte = when
            last_byte = when
            pending += data

        if driver.error:
            say("the script stopped early: %s" % driver.error)
    finally:
        stop.set()
        driver.stop.set()
        # Closing the console releases the reader from its blocking read, so the
        # teardown comes before the joins and no thread is left dangling.
        stack.stop()
        for thread in (reader_thread, driver_thread):
            if thread is not None:
                thread.join(timeout=5.0)
        try:
            flush()
        except Exception as error:
            print("warning: the last frame did not decode: %s" % error, file=sys.stderr)
        raw_stream.close()
        writer.close()

    return (
        (3 if driver.error else 0) if writer.records else 1,
        terminal,
        summarise(writer.records, terminal, time.perf_counter() - root, frames_path, raw_path)
        + (["script stopped: %s" % driver.error] if driver.error else []),
        run_dir,
        frames_path,
    )


def replay(options):
    """Re-decode a captured byte tape, for checking the grid without a stack."""
    root = time.perf_counter()
    terminal = Terminal(options.cols, options.rows)
    frames_path = options.frames_out or os.path.join(DEV, "live", "replay-frames.txt")
    writer = FrameWriter(frames_path, options.attrs, root)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    with open(options.replay, "rb") as handle:
        data = handle.read()
    previous = None
    step = max(1, options.replay_every)
    for index in range(0, len(data), step):
        chunk = data[index : index + step]
        terminal.feed(decoder.decode(chunk))
        if (index // step) % 20 == 0:
            previous = writer.emit(terminal, previous, None, None, len(chunk))
    terminal.feed(decoder.decode(b"", True))
    writer.emit(terminal, previous, None, None, 0)
    writer.close()
    lines = summarise(writer.records, terminal, time.perf_counter() - root, frames_path, options.replay)
    return 0, terminal, lines


def build_parser():
    parser = argparse.ArgumentParser(
        prog="tui_live.py",
        description="Drive the real gongga TUI in a pseudo-terminal and record its frames.",
    )
    parser.add_argument("--scenario", help="one of the shipped scenario names")
    parser.add_argument("--script", help="a file of script lines")
    parser.add_argument("--list-scenarios", action="store_true")
    parser.add_argument("--replay", help="decode a captured raw.bin instead of running anything")
    parser.add_argument("--replay-every", type=int, default=4096)
    parser.add_argument("--cols", type=int, default=100)
    parser.add_argument("--rows", type=int, default=30)
    parser.add_argument("--quiet-ms", type=float, default=20.0, help="bytes stop this long -> one frame")
    parser.add_argument("--type-delay-ms", type=float, default=40.0)
    parser.add_argument("--settle-timeout-ms", type=float, default=120000.0)
    parser.add_argument("--attrs", action="store_true", help="append per-row attribute codes")
    parser.add_argument("--frames-out", help="where the frames land")
    parser.add_argument("--stdout-frames", action="store_true", help="also dump the frame file at the end")
    parser.add_argument("--home", default=os.path.join(DEV, "live-home"), help="server home, inside the worktree")
    parser.add_argument("--keep-home", action="store_true", help="reuse the home instead of wiping it")
    parser.add_argument("--python", default=sys.executable, help="interpreter for the fake endpoint")
    parser.add_argument("--fake", choices=("plain", "stream"), default="stream")
    parser.add_argument("--stream-chunks", type=int, default=24)
    parser.add_argument("--stream-delay-ms", type=float, default=35.0)
    parser.add_argument("--tool", default="bash", help="empty string for no tool call")
    parser.add_argument("--tool-args", default=None)
    parser.add_argument("--model", default="fake/stream-model")
    parser.add_argument("--server-log", default="info")
    parser.add_argument("--server-exe", default=os.path.join(ROOT, "target", "debug", "gongga-server.exe"))
    parser.add_argument("--client-exe", default=os.path.join(ROOT, "target", "debug", "gongga-tui.exe"))
    parser.add_argument("--bash", default=_find_bash())
    parser.add_argument("--reasoning-lines", default=json.dumps(REASONING_LINES))
    parser.add_argument("--answer-lines", default=json.dumps(ANSWER_LINES))
    return parser


REASONING_LINES = [
    "The operator asked for an explanation of the event path, so read it once before answering.",
    "Reasoning arrives as its own part, which means its rows must not join the answer rows.",
    "A long sentence exists here only to make the stream long enough to watch the repaint cost.",
    "If the log window is a borrow of the log's own buffers, a frame should not allocate a string per row.",
]
ANSWER_LINES = [
    "The log is a window over blocks the client already holds, and each delta repaints the rows that moved.",
    "Streaming therefore costs a frame per quiet gap rather than a frame per byte, because the harness coalesces.",
    "When a tool call joins the turn its output arrives on its own keys, so it cannot reorder the answer.",
    "Two sessions in one window interleave by session tag, and that tag is part of the stream key.",
    "Anything that clears the screen before repainting would read as a flash, which is what this run counts.",
]


def _find_bash():
    for candidate in (
        os.environ.get("GONGGA_BASH_PATH"),
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
    ):
        if candidate and os.path.isfile(candidate):
            return candidate
    return None


def load_scenarios():
    path = os.path.join(ROOT, "scripts", "tui_live_scenarios.py")
    spec = importlib.util.spec_from_file_location("tui_live_scenarios", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.SCENARIOS


def main(argv=None):
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    parser = build_parser()
    options = parser.parse_args(argv)

    if options.list_scenarios:
        scenarios = load_scenarios()
        for name in sorted(scenarios):
            print("%s: %s" % (name, scenarios[name]["description"]))
        return 0

    if options.replay:
        code, terminal, lines = replay(options)
        print("\n".join(lines))
        print("final grid:\n%s" % "\n".join(terminal.rows_text()))
        return code

    if sys.platform != "win32":
        parser.error("the pseudo-console half of this harness is Windows only")

    if bool(options.scenario) == bool(options.script):
        parser.error("pass exactly one of --scenario or --script")
    if options.scenario:
        scenarios = load_scenarios()
        chosen = scenarios.get(options.scenario)
        if chosen is None:
            parser.error("unknown scenario %r; --list-scenarios shows them" % options.scenario)
        script = list(chosen["script"])
        overrides = chosen.get("options", {})
        for key, value in overrides.items():
            setattr(options, key.replace("-", "_"), value)
        print("scenario %s: %s" % (options.scenario, chosen["description"]))
    else:
        script = load_script(options.script)

    for name, path in (("server", options.server_exe), ("client", options.client_exe)):
        if not os.path.isfile(path):
            print("missing %s binary: %s (build it; this harness never runs cargo)" % (name, path), file=sys.stderr)
            return 2

    if os.path.isdir(options.home) and not options.keep_home:
        shutil.rmtree(options.home, ignore_errors=True)
        if os.path.isdir(options.home):
            print("warning: %s is still there after wiping; a server may be holding it" % options.home, file=sys.stderr)
    elif options.keep_home:
        print("reusing the home at %s" % options.home)
    elif not os.path.isdir(options.home):
        os.makedirs(options.home, exist_ok=True)

    code, terminal, lines, run_dir, frames_path = run_live(options, script)
    print("\n".join(lines))
    unknown = terminal.unknown_sequences
    if unknown:
        print("sequences the grid ignored: %s" % json.dumps(unknown, sort_keys=True))
    if code:
        print("no frames: the client wrote nothing")
    print("final grid:\n%s" % "\n".join(terminal.rows_text()))
    print("run directory: %s" % run_dir)
    if options.stdout_frames:
        with open(frames_path, "r", encoding="utf-8") as handle:
            sys.stdout.write(handle.read())
    return code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("interrupted; everything this run started was killed", file=sys.stderr)
        sys.exit(130)
