#!/usr/bin/env python3
"""A stand-in for an OpenAI-compatible Responses endpoint.

Used for smoke tests so a full run never has to reach a real provider:

    python scripts/fake_llm.py 8787
    GONGGA_BASE_URL=http://127.0.0.1:8787/v1 gongga-server --db ... --bash ...

It answers every request with a short reply, and optionally one tool call
first, so the tool path can be exercised too (set FAKE_LLM_TOOL=1).
"""

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import os

TOOL = {
    "call_id": "call_1",
    "name": "bash",
    # Override with FAKE_LLM_TOOL_ARGS, e.g.
    #   '{"cmd": "sleep 20", "detached": true}'
    "arguments": os.environ.get("FAKE_LLM_TOOL_ARGS") or '{"cmd":"echo hi-from-fake"}',
}


def events(with_tool: bool):
    seq = iter(range(1, 1000))

    def ev(kind, **payload):
        return {"event": kind, "data": {**payload, "sequence_number": next(seq)}}

    out = [
        ev("response.created", response={"id": "resp_1"}),
        ev("response.output_item.added", item={"id": "msg_1", "type": "message"}),
    ]
    if with_tool:
        out += [
            ev("response.output_item.added", item={"id": "fc_1", "type": "function_call", **TOOL}),
            ev("response.function_call_arguments.delta", item_id="fc_1", delta=TOOL["arguments"]),
            # `output_item.done` is what turns the streamed call into a tool
            # call the agent can execute.
            ev("response.output_item.done", item={"id": "fc_1", "type": "function_call", **TOOL}),
        ]
    out += [
        ev("response.output_item.added", item={"id": "msg_1", "type": "message"}),
        ev("response.output_text.delta", item_id="msg_1", delta="hello from the fake model"),
        ev("response.output_text.done", item_id="msg_1", text="hello from the fake model"),
        ev(
            "response.output_item.done",
            item={"id": "msg_1", "type": "message", "content": [{"type": "output_text", "text": "hello from the fake model"}]},
        ),
        ev("response.completed", response={"id": "resp_1"}),
    ]
    return out


class Handler(BaseHTTPRequestHandler):
    with_tool = False

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        for e in events(self.with_tool):
            self.wfile.write(f"event: {e['event']}\n".encode())
            self.wfile.write(f"data: {json.dumps(e['data'])}\n\n".encode())
            self.wfile.flush()
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    import os

    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    Handler.with_tool = os.environ.get("FAKE_LLM_TOOL") == "1"
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"fake llm on http://127.0.0.1:{port}/v1", flush=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()
