#!/usr/bin/env python3
"""A stand-in for an OpenAI-compatible Responses endpoint.

Used for smoke tests so a full run never has to reach a real provider:

    python scripts/fake_llm.py 8787
    GONGGA_BASE_URL=http://127.0.0.1:8787/v1 gongga-server --home .dev/fake-home --bash ...

Knobs, all environment variables:

- `FAKE_LLM_DUMP=<path>`: append every request body received to that file, one
  JSON object per line. This is how the smoke test shows what the server sent.
- `FAKE_LLM_HEADERS_DUMP=<path>`: the same for request headers, so an
  `X-Session-ID` affinity header can be checked without a real endpoint.
- `usage`: token counts are measured off the request, and `cached_tokens` is
  modelled as the leading token blocks of the *whole request* that are identical
  to what this endpoint holds. A real provider caches a token prefix quantized to
  its own block, not a sequence of units; see
  `docs/notes/server-prefix-cache.md`. `FAKE_LLM_INPUT_TOKENS` overrides the
  measured input count.

  Cache knobs, all environment variables:
  - `FAKE_LLM_CACHE_MODEL`: `token-prefix` (default) or `units` (the old model,
    kept so a change can be compared against it).
  - `FAKE_LLM_CACHE_BLOCK_TOKENS`: block size, default 128.
  - `FAKE_LLM_CACHE_TTL_SECS`: lifetime, 0 means no expiry.
  - `FAKE_LLM_CACHE_RENEWAL`: `renewing` (a hit resets the clock), `deadline`
    (the clock starts at the write), or `evicted` (no TTL to obey).
  - `FAKE_LLM_CACHE_MIN_TOKENS`: shortest prefix the endpoint will hold, 0 by
    default. A partial trailing block is never served from cache.
  - `FAKE_LLM_CACHE_TIERS`: JSON object keyed by `prompt_cache_options.ttl`, each
    value may override `ttl`, `renewal` and `minimum`.
  - `FAKE_LLM_CACHE_EVICT_PROBABILITY` / `FAKE_LLM_CACHE_SEED`: only for the
    `evicted` mode, which otherwise has no clock to obey.
- `FAKE_LLM_TOOL=<name>`: emit one function call for that tool on the first
  request of a run, then a plain message. `FAKE_LLM_TOOL_ARGS` is its arguments.
- `FAKE_LLM_WEB=1`: emit a provider-side `web_search_call` item plus a citation,
  which is what an OpenAI response with built-in web search looks like.
- `FAKE_LLM_IMAGE_GEN=1`: emit an `image_generation_call` carrying a base64
  `result`, which is what a provider-side image generation looks like -- and the
  one kind of item whose payload must not reach the transcript.
- `FAKE_LLM_FAIL_MODEL=<model id>`: answer `400` to any request whose `model`
  names it, and serve everything else. A 4xx is what a provider says about a
  request rather than a minute, so the harness does not retry it -- which makes
  this the way to reach a failed turn in one step instead of after a backoff.

State is per-process and keyed on how many requests have arrived, so "first
request calls a tool, the second answers" works without any real logic.
"""

import json
import os
import random
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DUMP = os.environ.get("FAKE_LLM_DUMP")
HEADERS_DUMP = os.environ.get("FAKE_LLM_HEADERS_DUMP")
TOOL_NAME = os.environ.get("FAKE_LLM_TOOL")
TOOL_ARGS = os.environ.get("FAKE_LLM_TOOL_ARGS") or (
    '{"bin":"bash","args":["-lc","echo hi-from-fake"]}'
)
WEB = os.environ.get("FAKE_LLM_WEB") == "1"
# A model id this endpoint refuses, which is how a failed turn is produced fast.
FAIL_MODEL = os.environ.get("FAKE_LLM_FAIL_MODEL") or ""
# What the fake claims the request cost, so a threshold can be crossed on purpose.
INPUT_TOKENS = int(os.environ.get("FAKE_LLM_INPUT_TOKENS") or 0)

CACHE_MODEL = os.environ.get("FAKE_LLM_CACHE_MODEL") or "token-prefix"
CACHE_BLOCK_TOKENS = max(1, int(os.environ.get("FAKE_LLM_CACHE_BLOCK_TOKENS") or 128))
CACHE_TTL_SECS = float(os.environ.get("FAKE_LLM_CACHE_TTL_SECS") or 0)
CACHE_RENEWAL = os.environ.get("FAKE_LLM_CACHE_RENEWAL") or "renewing"
CACHE_MIN_TOKENS = int(os.environ.get("FAKE_LLM_CACHE_MIN_TOKENS") or 0)
CACHE_TIERS = json.loads(os.environ.get("FAKE_LLM_CACHE_TIERS") or "{}")
CACHE_EVICT_PROBABILITY = float(os.environ.get("FAKE_LLM_CACHE_EVICT_PROBABILITY") or 0)
_RNG = random.Random(int(os.environ.get("FAKE_LLM_CACHE_SEED") or 0))

# What this endpoint holds: complete token blocks of the last request, and the
# two clocks a tier's renewal semantics read.
_held_blocks = []
_written_at = 0.0
_touched_at = 0.0
_previous_units = []


def units(raw):
    """The request's parts, in the order its prefix puts them on the wire."""
    try:
        body = json.loads(raw.decode("utf-8", "replace"))
    except ValueError:
        return []
    out = [
        json.dumps(body.get("tools") or [], sort_keys=True, ensure_ascii=False),
        json.dumps(body.get("instructions") or "", sort_keys=True, ensure_ascii=False),
    ]
    out += [
        json.dumps(item, sort_keys=True, ensure_ascii=False)
        for item in (body.get("input") or [])
    ]
    return out


def cache_option(raw):
    """The cache directive this request named, if it named one."""
    try:
        body = json.loads(raw.decode("utf-8", "replace"))
    except ValueError:
        return {}
    option = body.get("prompt_cache_options")
    return option if isinstance(option, dict) else {}


def measure(units_now, option=None):
    """(input, cached, written) against what this endpoint holds.

    A provider caches a token prefix of the whole request, quantized to complete
    blocks: a change keeps everything before it and nothing after it.
    """
    global _held_blocks, _written_at, _touched_at, _previous_units
    chars = sum(len(u) for u in units_now)
    input_tokens = INPUT_TOKENS or max(chars // 4, 1)

    if CACHE_MODEL == "units":
        hit = 0
        for a, b in zip(_previous_units, units_now):
            if a != b:
                break
            hit += len(a)
        _previous_units = units_now
        return (input_tokens, hit // 4, 0)

    settings = CACHE_TIERS.get((option or {}).get("ttl", "default"), {})
    ttl = float(settings.get("ttl", CACHE_TTL_SECS))
    renewal = settings.get("renewal", CACHE_RENEWAL)
    minimum = int(settings.get("minimum", CACHE_MIN_TOKENS))

    now = time.monotonic()
    width = CACHE_BLOCK_TOKENS * 4
    whole = "".join(units_now)
    request_blocks = [whole[i:i + width] for i in range(0, len(whole), width)]
    # A partial trailing block is not in the cache and cannot be served from it.
    if request_blocks and len(request_blocks[-1]) < width:
        request_blocks.pop()

    expired = False
    if ttl > 0 and _held_blocks and renewal == "deadline":
        expired = now - _written_at > ttl
    if ttl > 0 and _held_blocks and renewal == "renewing":
        expired = now - _touched_at > ttl
    if renewal == "evicted" and CACHE_EVICT_PROBABILITY > 0:
        expired = expired or _RNG.random() < CACHE_EVICT_PROBABILITY
    if expired:
        _held_blocks = []

    common = 0
    for a, b in zip(_held_blocks, request_blocks):
        if a != b:
            break
        common += 1
    cached = common * CACHE_BLOCK_TOKENS
    written = 0

    if input_tokens >= minimum:
        written = max(0, len(request_blocks) - common) * CACHE_BLOCK_TOKENS
        fresh = not _held_blocks
        if fresh:
            _written_at = now
            _touched_at = now
        elif renewal == "renewing" and cached:
            _touched_at = now
        _held_blocks = list(request_blocks)
    else:
        _held_blocks = []

    return (input_tokens, cached, written)
IMAGE_GEN = os.environ.get("FAKE_LLM_IMAGE_GEN") == "1"

_lock = threading.Lock()
_requests = 0
_uploads = 0
# The file ids this fake is holding. `FAKE_LLM_FORGET_FILES_AFTER=n` drops all of them
# after the n-th /responses request, which is how a provider expiring a file is modeled.
UPLOADED: set[str] = set()
FILES_LOG = os.environ.get("FAKE_LLM_FILES_LOG") or ""
FORGET_AFTER = int(os.environ.get("FAKE_LLM_FORGET_FILES_AFTER") or 0)
# A slow answer, in pieces: `piece|piece|...` and the pause between them.
SLOW_PIECES = (os.environ.get("FAKE_LLM_SLOW_PIECES") or "").split("|") if os.environ.get("FAKE_LLM_SLOW_PIECES") else []
SLOW_PAUSE = float(os.environ.get("FAKE_LLM_SLOW_PAUSE") or 0)
# A thought streamed the same way, so a kill can land inside one.
SLOW_REASONING = (os.environ.get("FAKE_LLM_SLOW_REASONING") or "").split("|") \
    if os.environ.get("FAKE_LLM_SLOW_REASONING") else []


def events(with_tool: bool, with_web: bool, with_image: bool, usage=(0, 0, 0)):
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
    # A thought, streamed the same way, so a kill can land inside one.
    if SLOW_REASONING:
        out += [
            ev("response.output_item.added",
               item={"id": "rs_1", "type": "reasoning", "status": "in_progress"}),
        ]
        for piece in SLOW_REASONING:
            out += [
                ev("response.reasoning_text.delta", item_id="rs_1", delta=piece),
                {"event": "fake.pause", "data": {"seconds": SLOW_PAUSE}},
            ]
        out += [
            ev("response.reasoning_text.done", item_id="rs_1",
               text="".join(SLOW_REASONING)),
            ev("response.output_item.done",
               item={"id": "rs_1", "type": "reasoning", "status": "completed",
                     "content": [{"type": "reasoning_text",
                                  "text": "".join(SLOW_REASONING)}]}),
        ]
    pieces = SLOW_PIECES or ["hello from the fake model"]
    out += [ev("response.output_item.added", item={"id": "msg_1", "type": "message"})]
    for piece in pieces:
        out += [
            ev("response.output_text.delta", item_id="msg_1", delta=piece),
            # A marker the handler turns into a pause, so a kill lands mid-answer.
            {"event": "fake.pause", "data": {"seconds": SLOW_PAUSE}},
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
    whole = "".join(pieces)
    out += [
        ev("response.output_text.done", item_id="msg_1", text=whole),
        ev(
            "response.output_item.done",
            item={
                "id": "msg_1",
                "type": "message",
                "content": [{"type": "output_text", "text": whole}],
            },
        ),
        ev(
            "response.completed",
            response={
                "id": "resp_1",
                "usage": {
                    "input_tokens": usage[0],
                    # Nested exactly as the Responses API spells it, because that is
                    # the shape the harness has to read. A real Responses endpoint
                    # reports zero here even on a cold write; the fake reports the
                    # tokens it wrote so a cost model can be exercised offline.
                    "input_tokens_details": {
                        "cached_tokens": usage[1],
                        "cache_write_tokens": usage[2],
                    },
                    "output_tokens": 1,
                    "total_tokens": usage[0] + 1,
                },
            },
        ),
    ]
    return out


class Handler(BaseHTTPRequestHandler):
    def request_body(self):
        if "chunked" not in (self.headers.get("Transfer-Encoding") or "").lower():
            length = int(self.headers.get("Content-Length") or 0)
            return self.rfile.read(length) if length else b""
        chunks = []
        while True:
            size = int(self.rfile.readline().strip().split(b";", 1)[0], 16)
            if size == 0:
                while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                    pass
                return b"".join(chunks)
            chunks.append(self.rfile.read(size))
            self.rfile.read(2)

    def do_POST(self):  # noqa: N802
        global _requests
        if FORGET_AFTER and _requests == FORGET_AFTER:
            UPLOADED.clear()
        raw = self.request_body()
        # The Files API is a different shape from the Responses stream: one JSON
        # id back, nothing to stream. Without this the harness's upload would be
        # answered with an SSE body and read as "no file id".
        if self.path.rstrip("/").endswith("/files"):
            # One id per upload, counted: a caller that uploads the same picture twice
            # gets two ids, which is how a reuse bug becomes visible.
            global _uploads
            _uploads += 1
            file_id = f"file-fake-{_uploads}"
            UPLOADED.add(file_id)
            with open(FILES_LOG, "a", encoding="utf-8") as log:
                log.write(file_id + "\n")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {
                        "id": file_id,
                        "object": "file",
                        "bytes": len(raw),
                        "purpose": "user_data",
                        "created_at": int(time.time()),
                        "expires_at": int(time.time()) + 7 * 24 * 3600,
                    }
                ).encode()
            )
            return
        if self.command == "DELETE" and "/files/" in self.path:
            UPLOADED.discard(self.path.rsplit("/", 1)[-1])
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"deleted": true}')
            return
        # A file this endpoint no longer holds: the shape a real provider answers with
        # when its file expired or was deleted. Named here so the harness can prove it
        # forgets the id and uploads again instead of failing the turn for ever.
        named = set(re.findall(r"file-fake-\d+", raw.decode("utf-8", "replace")))
        missing = named - UPLOADED
        if missing:
            detail = json.dumps(
                {
                    "error": {
                        "message": f"file {sorted(missing)[0]} has expired or does not exist",
                        "type": "invalid_request_error",
                        "code": "file_not_found",
                    }
                }
            ).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(detail)))
            self.end_headers()
            self.wfile.write(detail)
            return
        # The `model` field, never the body: a transcript may name the model that
        # is being refused, and a match on the text would refuse the wrong request.
        def names_failing_model():
            try:
                return json.loads(raw.decode("utf-8", "replace")).get("model") == FAIL_MODEL
            except ValueError:
                return False

        if FAIL_MODEL and names_failing_model():
            body = json.dumps(
                {
                    "error": {
                        "message": f"this endpoint does not serve {FAIL_MODEL}",
                        "type": "invalid_request_error",
                        "code": "model_not_served",
                    }
                }
            ).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if DUMP:
            with _lock:
                with open(DUMP, "a", encoding="utf-8") as f:
                    f.write(raw.decode("utf-8", "replace").replace("\n", " ") + "\n")
        if HEADERS_DUMP:
            with _lock:
                with open(HEADERS_DUMP, "a", encoding="utf-8") as f:
                    f.write(json.dumps(dict(self.headers)) + "\n")
        with _lock:
            _requests += 1
            first = _requests == 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        for e in events(first and bool(TOOL_NAME), WEB, IMAGE_GEN,
                        measure(units(raw), cache_option(raw))):
            if e["event"] == "fake.pause":
                # Not a protocol event: a delay, so a client can be killed mid-stream.
                time.sleep(float(e["data"]["seconds"]))
                continue
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
