#!/usr/bin/env python3
"""A stand-in for an OpenAI-compatible Responses endpoint.

Used for smoke tests so a full run never has to reach a real provider:

    python scripts/fake_llm.py 8787
    GONGGA_BASE_URL=http://127.0.0.1:8787/v1 gongga-server --db ... --bash ...

Knobs, all environment variables:

- `FAKE_LLM_DUMP=<path>`: append every request body received to that file, one
  JSON object per line. This is how the smoke test shows what the server sent.
- `FAKE_LLM_TOOL=<name>`: emit one function call for that tool on the first
  request of a run, then a plain message. `FAKE_LLM_TOOL_ARGS` is its arguments.
- `FAKE_LLM_WEB=1`: emit a provider-side `web_search_call` item plus a citation,
  which is what an OpenAI response with built-in web search looks like.
- `FAKE_LLM_IMAGE_GEN=1`: emit an `image_generation_call` carrying a base64
  `result`, which is what a provider-side image generation looks like -- and the
  one kind of item whose payload must not reach the transcript.

State is per-process and keyed on how many requests have arrived, so "first
request calls a tool, the second answers" works without any real logic.
"""

import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DUMP = os.environ.get("FAKE_LLM_DUMP")
TOOL_NAME = os.environ.get("FAKE_LLM_TOOL")
TOOL_ARGS = os.environ.get("FAKE_LLM_TOOL_ARGS") or '{"command":"echo hi-from-fake"}'
WEB = os.environ.get("FAKE_LLM_WEB") == "1"
# What the fake claims the request cost, so a threshold can be crossed on purpose.
INPUT_TOKENS = int(os.environ.get("FAKE_LLM_INPUT_TOKENS") or 0)
IMAGE_GEN = os.environ.get("FAKE_LLM_IMAGE_GEN") == "1"

_lock = threading.Lock()
_requests = 0


def events(with_tool: bool, with_web: bool, with_image: bool):
    seq = iter(range(1, 100000))

    def ev(kind, **payload):
        return {"event": kind, "data": {**payload, "sequence_number": next(seq)}}

    out = [ev("response.created", response={"id": "resp_1"})]
    if with_tool:
        call = {
            "call_id": "call_1",
            "name": TOOL_NAME,
            "arguments": TOOL_ARGS,
        }
        out += [
            ev("response.output_item.added", item={"id": "fc_1", "type": "function_call", **call}),
            ev("response.function_call_arguments.delta", item_id="fc_1", delta=TOOL_ARGS),
            # `output_item.done` is what turns the streamed call into a tool
            # call the agent can execute.
            ev("response.output_item.done", item={"id": "fc_1", "type": "function_call", **call}),
        ]
    if with_web:
        search = {
            "id": "ws_1",
            "type": "web_search_call",
            "status": "in_progress",
            "action": {"type": "search", "query": "rust async book"},
        }
        out += [
            ev("response.output_item.added", item=search),
            ev("response.web_search_call.searching", item_id="ws_1"),
            ev(
                "response.output_item.done",
                item={**search, "status": "completed"},
            ),
        ]
    if with_image:
        gen = {
            "id": "ig_1",
            "type": "image_generation_call",
            "status": "in_progress",
            "revised_prompt": "a red cube on a table",
        }
        out += [
            ev("response.output_item.added", item=gen),
            ev("response.image_generation_call.generating", item_id="ig_1"),
            ev(
                "response.output_item.done",
                item={**gen, "status": "completed", "result": "iVBORw0KGgoAAAANSUhEUg=="},
            ),
        ]
    out += [
        ev("response.output_item.added", item={"id": "msg_1", "type": "message"}),
        ev("response.output_text.delta", item_id="msg_1", delta="hello from the fake model"),
    ]
    if with_web:
        out += [
            ev(
                "response.output_text.annotation.added",
                item_id="msg_1",
                annotation={
                    "type": "url_citation",
                    "url": "https://example.org/async-rust",
                    "title": "Async Rust",
                    "start_index": 0,
                    "end_index": 5,
                },
            ),
        ]
    out += [
        ev("response.output_text.done", item_id="msg_1", text="hello from the fake model"),
        ev(
            "response.output_item.done",
            item={
                "id": "msg_1",
                "type": "message",
                "content": [{"type": "output_text", "text": "hello from the fake model"}],
            },
        ),
        ev(
            "response.completed",
            response={
                "id": "resp_1",
                "usage": {
                    "input_tokens": INPUT_TOKENS,
                    "output_tokens": 1,
                    "total_tokens": INPUT_TOKENS + 1,
                },
            },
        ),
    ]
    return out


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802
        global _requests
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        if DUMP:
            with _lock:
                with open(DUMP, "a", encoding="utf-8") as f:
                    f.write(raw.decode("utf-8", "replace").replace("\n", " ") + "\n")
        with _lock:
            _requests += 1
            first = _requests == 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        for e in events(first and bool(TOOL_NAME), WEB, IMAGE_GEN):
            self.wfile.write(f"event: {e['event']}\n".encode())
            self.wfile.write(f"data: {json.dumps(e['data'])}\n\n".encode())
            self.wfile.flush()
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"fake llm on http://127.0.0.1:{port}", flush=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()
