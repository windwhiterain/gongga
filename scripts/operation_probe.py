#!/usr/bin/env python3
"""Send one protocol operation to a server and print what it answers.

The window's gestures are operations, not command lines, so the wire carries
them beside the commands. This speaks the channel protocol directly -- SYN, then
the operation, then the answer -- so the new message kind is exercised by a real
process rather than by a unit of the client.

    python scripts/operation_probe.py --port 9700 --key <key> delete_session --session main

Exits 0 when the server answered ok, 1 when it refused, 2 when it did not
answer at all.
"""

import argparse
import json
import socket
import struct
import sys
import time

MAGIC = 0x4747
VERSION = 1
HEADER_LEN = 26
SYN = 0x01
ACK = 0x02
DATA = 0x10
WINDOW = 64


def frame(flags, conn_id, seq, ack, msg_id, frag_idx, frag_cnt, payload=b""):
    head = struct.pack(
        ">HBBIIIIHHH", MAGIC, VERSION, flags, conn_id, seq, ack, msg_id,
        frag_idx, frag_cnt, WINDOW,
    )
    return head + payload


def read_frames(sock, timeout):
    sock.settimeout(timeout)
    out = []
    try:
        while True:
            data = sock.recv(65535)
            if not data:
                break
            if len(data) < HEADER_LEN:
                continue
            (_magic, _version, flags, conn_id, seq, ack, msg_id, frag_idx,
             frag_cnt, _window) = struct.unpack(">HBBIIIIHHH", data[:HEADER_LEN])
            out.append((flags, conn_id, seq, ack, msg_id, frag_idx, frag_cnt,
                        data[HEADER_LEN:]))
    except socket.timeout:
        pass
    return out


def wait_for(sock, timeout, wanted):
    """The messages that arrive until one of them answers what was asked for.

    A message can be fragmented -- the greeting carries the whole catalog --
    so the frames are put back together by their message id before they are
    read.
    """
    deadline = time.time() + timeout
    parts = {}
    while time.time() < deadline:
        for flags, conn_id, seq, ack, msg_id, frag_idx, frag_cnt, payload in read_frames(sock, 0.5):
            if not (flags & DATA):
                continue
            piece = parts.setdefault(msg_id, {"count": frag_cnt, "chunks": {}})
            piece["count"] = frag_cnt
            piece["chunks"][frag_idx] = payload
            if len(piece["chunks"]) < piece["count"]:
                continue
            whole = b"".join(piece["chunks"][index] for index in range(piece["count"]))
            try:
                message = json.loads(whole.decode("utf-8"))
            except ValueError:
                continue
            if wanted(message):
                return message, conn_id
            # Whatever else the server said is the answer to "why not".
            print("server said: %s" % json.dumps(message, ensure_ascii=False)[:400],
                  file=sys.stderr)
    return None, None


def event_of(message):
    """The event a message carries: an event is `{"t": "event", "v": {"seq", "event"}}`."""
    payload = message.get("v", {})
    return payload.get("event", payload)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--key", required=True)
    parser.add_argument("--session", default="")
    parser.add_argument("--show", default="")
    parser.add_argument("--timeout", type=float, default=5.0)
    parser.add_argument("operation", choices=["delete_session", "sticky_session",
                                              "render_session"])
    args = parser.parse_args()

    if args.operation == "delete_session":
        operation = {"op": "delete_session", "session": args.session}
    elif args.operation == "sticky_session":
        operation = {"op": "sticky_session", "session": args.session}
    else:
        operation = {"op": "render_session", "session": args.session,
                     "show": args.show.lower() == "true"}

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.connect((args.host, args.port))
    sock.send(frame(SYN, 0, 0, 0, 0, 0, 0))

    conn_id = None
    deadline = time.time() + args.timeout
    while time.time() < deadline and conn_id is None:
        sock.settimeout(0.5)
        try:
            data = sock.recv(65535)
        except socket.timeout:
            continue
        if len(data) < HEADER_LEN:
            continue
        (_magic, _version, flags, cid, _seq, _ack, _msg_id, _frag_idx,
         _frag_cnt, _window) = struct.unpack(">HBBIIIIHHH", data[:HEADER_LEN])
        if flags & SYN:
            conn_id = cid
    if conn_id is None:
        print("the server never answered the handshake", file=sys.stderr)
        return 2

    hello = json.dumps({
        "t": "hello",
        "client": {"version": "probe", "build": "probe", "label": "probe"},
        "key": args.key,
    }).encode("utf-8")
    sock.send(frame(DATA | ACK, conn_id, 1, 0, 1, 0, 1, hello))

    greeting, _ = wait_for(sock, args.timeout, lambda m: m.get("t") == "hello")
    if greeting is None:
        print("the server never sent its greeting", file=sys.stderr)
        return 2

    operation_bytes = json.dumps({
        "t": "operation", "id": 7, "operation": operation
    }).encode("utf-8")
    sock.send(frame(DATA | ACK, conn_id, 2, 1, 2, 0, 1, operation_bytes))

    answer, _ = wait_for(
        sock, args.timeout,
        lambda m: event_of(m).get("t") == "cmd_result" and event_of(m).get("id") == 7,
    )
    if answer is None:
        print("the server did not answer the operation", file=sys.stderr)
        return 2
    print(json.dumps(event_of(answer), ensure_ascii=False, separators=(",", ":")))
    return 0 if event_of(answer).get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
