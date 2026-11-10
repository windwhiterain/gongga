"""A client for the harness, with one function per command.

The harness is a server that answers commands by name; this module puts those
names on an object, so a Python program says what it means instead of building a
command line and reading JSON off stdout:

    import gongga

    server = gongga.connect()                       # from GONGGA_SERVER / the home
    server.set_session(id="main", cwd="/tmp")       # a command, its arguments by name
    server.send_message(session="main", message="go")  # refuse -> GonggaError, with the reason
    for session in server.sessions:                 # what the server published
        print(session["name"])

    server.call("get_messages", session="main", range="-5:")   # a Reply, not an exception

Which names exist is the server's answer, not this file's: the greeting carries
the catalog, and a name it does not publish raises `AttributeError` here rather
than travelling to the server to be refused. That is the same rule the command
line follows -- this module knows no command.

Arguments are the command's own: a string is a string, a list arrives as JSON for
an argument the schema types as a list, and an argument the server requires must
be present, or the server says so. The one convenience is `session`: a command
that acts on a session takes it by name like any other argument.

The four commands SPEC's `## Python Cmd` names are commands like the rest, so a
package skill calls them the same way:

    server.ls_skill(path="harness", session="main")        # -> its id, bucket, SKILL.md
    server.run_skill(id="harness/prefix-cache",            # one function of a package
                     cmd={"name": "look", "args": {"path": "/tmp"}},
                     session="main")
    server.get_messages(session="main", range="-20:-1")    # a range of the transcript
    server.clone_session(id="main", new_id="main-copy")    # a copy that runs its own turns
"""

import json
import os
import socket
import struct
import time

#: The channel's frame: magic, version, flags, connection, sequence, ack, message,
#: fragment index, fragment count, window. See `docs/notes/protocol-frame.md`.
MAGIC = 0x4747
VERSION = 1
HEADER = 26
SYN = 0x01
ACK = 0x02
DATA = 0x10
WINDOW = 64

#: The default port a server binds when nothing names one.
DEFAULT_PORT = 9700


class GonggaError(Exception):
    """The server answered, and the answer was no."""

    def __init__(self, message, reply=None):
        super().__init__(message)
        self.reply = reply


class Unreachable(GonggaError):
    """The server did not answer at all: nothing to refuse, nothing to retry."""


def _frame(flags, connection, sequence, ack, message, index, count, payload=b""):
    head = struct.pack(
        ">HBBIIIIHHH", MAGIC, VERSION, flags, connection, sequence, ack, message,
        index, count, WINDOW,
    )
    return head + payload


def _wire(value):
    """One argument as the wire spells it: every argument travels as a string.

    # Invariant

    A `cmd` is an object here and a string there, and so is a list of words: what a
    caller means as a structure is JSON, which is what the argument's own schema says
    it accepts. A string is itself, never quoted twice.
    """
    if isinstance(value, str):
        return value
    return json.dumps(value, ensure_ascii=False)


class Reply:
    """What one command answered: `ok`, the message, and any rows it carried."""

    def __init__(self, ok, message, data=None, events=None):
        self.ok = ok
        self.message = message
        self.data = data
        self.events = events or []

    def __bool__(self):
        return self.ok

    def __str__(self):
        return self.message

    def __repr__(self):
        return f"Reply(ok={self.ok}, message={self.message!r})"

    def raise_for_status(self):
        """A refusal is this call's exception, carrying the server's own words."""
        if not self.ok:
            raise GonggaError(self.message, self)
        return self


class Client:
    """One connection: a handshake, then commands by name until `close`."""

    def __init__(self, port, key, host="127.0.0.1", timeout=30.0, label="python"):
        self.address = (host, int(port))
        self.key = key
        self.timeout = timeout
        self.label = label
        self.sessions = []
        self.cursor = None
        self.models = []
        self.default_model = None
        self.catalog = []
        self.pid = 0
        self._sock = None
        self._connection = 0
        self._sequence = 0
        self._message = 0
        # The next sequence this side expects, and the ones that arrived past a gap.
        # A cumulative ack is the next one expected, never the highest number seen.
        self._expected = 1
        self._ahead = set()
        self._next_id = 1
        self._greeted = False

    # -- connection ---------------------------------------------------------

    def connect(self):
        if self._sock is not None:
            return self
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.connect(self.address)
        self._sock = sock
        sock.send(_frame(SYN, 0, 0, 0, 0, 0, 0))
        deadline = time.time() + self.timeout
        while time.time() < deadline and not self._connection:
            sock.settimeout(0.5)
            try:
                data = sock.recv(65535)
            except socket.timeout:
                continue
            except OSError as reset:
                # A port with nothing behind it answers with ICMP, and Windows hands
                # that up as a reset: "nothing answered" is this loop's own answer,
                # not a traceback out of a library.
                sock.close()
                self._sock = None
                raise Unreachable(
                    f"nothing answered at {self.address[0]}:{self.address[1]} ({reset})"
                ) from None
            head = self._decode(data)
            if head is not None and head[2] & SYN:
                self._connection = head[3]
        if not self._connection:
            sock.close()
            self._sock = None
            raise Unreachable(f"nothing answered at {self.address[0]}:{self.address[1]}")
        # The first data frame is sequence 1: `_send` counts it.
        self._sequence = 0
        self._send({"t": "hello", "client": {
            "version": "python", "build": "python", "label": self.label,
            # Which session this client speaks for: a command notices it (a
            # subscription is armed on the session its shell belongs to), and a
            # client that says nothing is a client nobody can arm.
            "session": os.environ.get("GONGGA_SESSION", ""),
        }, "key": self.key})
        greeting = self._wait(lambda message: message.get("t") == "hello")
        if greeting is None:
            raise Unreachable("the server never answered the handshake")
        self._apply(greeting)
        self._greeted = True
        return self

    def close(self):
        if self._sock is not None:
            self._sock.close()
            self._sock = None
            self._connection = 0

    def __enter__(self):
        return self.connect()

    def __exit__(self, *_):
        self.close()

    # -- commands -----------------------------------------------------------

    def call(self, command, /, **arguments):
        """Ask for one command and answer with its `Reply`, refused or not.

        The name is positional only, so a command that has an argument called `name`
        -- `set_session` does -- spells it like any other. An argument that is not a
        string is sent as JSON, which is how a caller passes a `cmd` object.
        """
        self.connect()
        wanted = self._next_id
        self._next_id += 1
        self._send({
            "t": "command",
            "id": wanted,
            "name": command,
            "args": {key: _wire(value) for key, value in arguments.items()},
        })

        def answered(message):
            inner, _ = _event(message)
            return inner.get("t") == "cmd_result" and inner.get("id") == wanted

        seen = []
        while True:
            message = self._wait(answered, collect=seen)
            if message is None:
                raise Unreachable(f"{command} was sent and never answered")
            # The answer is a row of the stream too, and a caller that prints what it
            # read has to see it: `ok`, `message` and `data` are the command's words.
            seen.append(message)
            inner, _ = _event(message)
            return Reply(inner.get("ok", False), inner.get("message", ""),
                         inner.get("data"), seen)

    def pump(self, seconds=0.2):
        """Read what the server has said since the last call, and answer it.

        A command's reply is not the only thing on the link: a session list, a
        roster, a turn's events arrive beside it. `call` returns on its own reply,
        so a caller that wants the newest `sessions` asks for it.

        # Invariant
        #
        # Bounded: this reads for `seconds` and returns. Nothing here waits for a
        # message -- that is `call`'s job, and a pump that blocked would hang a loop
        # that only wanted the state a moment fresher.
        """
        self.connect()
        seen = []
        end = time.time() + seconds
        while time.time() < end:
            self._sock.settimeout(max(0.01, end - time.time()))
            try:
                data = self._sock.recv(65535)
            except socket.timeout:
                break
            if len(data) < HEADER:
                continue
            message = self._whole(data, {})
            if message is None:
                continue
            self._apply(message)
            seen.append(message)
        return seen

    def __getattr__(self, name):
        """`server.set_session(...)` for every name the greeting published."""
        if name.startswith("_"):
            raise AttributeError(name)
        if self._greeted and name not in {row["name"] for row in self.catalog}:
            raise AttributeError(
                f"this server publishes no {name!r}; it publishes: "
                + ", ".join(row["name"] for row in self.catalog)
            )

        def command(**arguments):
            return self.call(name, **arguments).raise_for_status()

        command.__name__ = name
        command.__doc__ = next(
            (row.get("description") for row in self.catalog if row["name"] == name), None
        )
        return command

    # -- the wire -----------------------------------------------------------

    def _send(self, message):
        # One frame, one sequence and one message id: the channel drops a frame that
        # repeats either, so reusing them is a command the server never sees.
        payload = json.dumps(message, ensure_ascii=False).encode("utf-8")
        self._sequence += 1
        self._message += 1
        self._sock.send(_frame(DATA | ACK, self._connection, self._sequence,
                               self._expected, self._message, 0, 1, payload))

    @staticmethod
    def _decode(data):
        if len(data) < HEADER:
            return None
        return struct.unpack(">HBBIIIIHHH", data[:HEADER])

    def _acknowledge(self, sequence):
        """Books one received sequence and tells the peer how far this side has it.

        # Invariant

        `_expected` is the next sequence expected, so the ack is cumulative and an
        out-of-order frame retires nothing. The channel retransmits what is not
        acknowledged and gives up when it has retransmitted enough, so a client
        waiting with nothing to send -- one command in flight, no reply yet -- must
        still answer: without this its connection dies after about a second and a
        slow command loses the answer it was waiting for.
        """
        if sequence == self._expected:
            self._expected = (self._expected + 1) & 0xFFFFFFFF
            while self._expected in self._ahead:
                self._ahead.discard(self._expected)
                self._expected = (self._expected + 1) & 0xFFFFFFFF
        elif (sequence - self._expected) & 0xFFFFFFFF < 0x80000000:
            # Past a gap: remembered, not acknowledged.
            self._ahead.add(sequence)
        self._sock.send(_frame(ACK, self._connection, 0, self._expected, 0, 0, 1))

    def _whole(self, data, parts):
        """One frame in, one whole message out when its fragments are in."""
        (magic, version, flags, connection, sequence, _ack, message_id,
         index, count, _window) = struct.unpack(">HBBIIIIHHH", data[:HEADER])
        if magic != MAGIC or version != VERSION or not flags & DATA:
            return None
        self._acknowledge(sequence)
        piece = parts.setdefault(message_id, {})
        piece[index] = data[HEADER:]
        if len(piece) < count:
            return None
        whole = b"".join(piece[at] for at in range(count))
        del parts[message_id]
        try:
            return json.loads(whole.decode("utf-8"))
        except ValueError:
            return None

    def _wait(self, wanted, collect=None):
        """The first message `wanted` accepts, and every other one seen first."""
        parts = {}
        deadline = time.time() + self.timeout
        while time.time() < deadline:
            self._sock.settimeout(0.5)
            try:
                data = self._sock.recv(65535)
            except socket.timeout:
                continue
            if len(data) < HEADER:
                continue
            message = self._whole(data, parts)
            if message is None:
                continue
            if wanted(message):
                return message
            if collect is not None:
                collect.append(message)
            keep, _ = _event(message)
            if keep.get("t") in ("session", "session_gone", "models"):
                self._apply(message)
        return None

    def _apply(self, message):
        """What the greeting and its republishing say about this server.

        Two envelopes travel: a greeting names its kind on the outside
        (`{"t": "hello", "v": {...}}`), an event names it inside (`{"v": {"t": ...}}`).
        A reader that takes one for the other sees an empty catalog and a server with
        no pid -- which is what an unreachable server looks like, wrongly.
        """
        kind = message.get("t")
        if kind == "hello":
            payload = message.get("v", message)
            self.pid = payload.get("server", {}).get("pid", 0)
            self.catalog = payload.get("commands", [])
            self.models = payload.get("models", [])
            self.default_model = payload.get("default_model")
            self.sessions = payload.get("sessions", [])
            # Where this link's stream starts: every event after this greeting is
            # numbered above it, so a jump in the numbers is a gap.
            self.cursor = payload.get("cursor")
            return
        event, _ = _event(message)
        if event.get("t") == "session":
            row = event.get("session", {})
            name = row.get("name")
            rest = [s for s in self.sessions if s.get("name") != name]
            self.sessions = [row] + rest
        elif event.get("t") == "session_gone":
            name = event.get("name")
            self.sessions = [s for s in self.sessions if s.get("name") != name]


def _event(message):
    """The event a message carries, and its place in the stream.

    Two envelopes travel and an event has one more layer than it used to: an
    event is `{"t": "event", "v": {"seq": n, "event": {...}}}`. `seq` is the
    server's stream cursor -- absent when the event is only this connection's
    answer -- and a gap in it says the server published something this client
    never received.
    """
    payload = message.get("v", message)
    if message.get("t") == "event":
        return payload.get("event", {}), payload.get("seq")
    return payload, None


def connect(port=None, key=None, host=None, home=None, timeout=30.0):
    """A connected client: `GONGGA_SERVER`/`GONGGA_KEY`, or the home's server."""
    home = home or os.environ.get("GONGGA_HOME") or os.path.join(os.path.expanduser("~"), ".gongga")
    endpoint = os.environ.get("GONGGA_SERVER", "")
    if port is None:
        port = int(endpoint.rsplit(":", 1)[1]) if endpoint.count(":") == 1 else DEFAULT_PORT
    if host is None:
        host = endpoint.rsplit(":", 1)[0] if endpoint.count(":") == 1 else "127.0.0.1"
    if key is None:
        key = os.environ.get("GONGGA_KEY")
    if key is None:
        with open(os.path.join(home, "server.key"), encoding="utf-8") as handle:
            key = handle.read().strip()
    return Client(port, key, host=host, timeout=timeout).connect()
