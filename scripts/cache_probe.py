#!/usr/bin/env python3
"""Measure what one provider's cache tiers actually do on the wire.

The roster and `docs/reference/prefix-cache.md` state what a vendor *says*; this
script records what the endpoint answers. It is a measurement, not a test: it
asserts nothing about correctness and always reports what it observed.

This sends real requests against a paid endpoint. For policy work, prefer the
local simulator in `scripts/fake_llm.py` with its `FAKE_LLM_CACHE_*` knobs, or
pass `--dry-run` here to see the request shapes without sending anything.

    python scripts/cache_probe.py --dry-run
    python scripts/cache_probe.py --only ttl --ttl-wait 1200
    python scripts/cache_probe.py --base-url https://api.kimi.com/coding/v1 \
        --model kimi-for-coding --key-env KIMI_CODING_API_KEY

Environment:
  KIMI_CODING_API_KEY   the credential, never printed
  CACHE_PROBE_BASE_URL  overrides --base-url

Three questions, each its own scenario:
  shape   which surface accepts the request, and whether a cache directive is
          acknowledged at all (an unknown field is often silently ignored)
  write   whether a write is billed on the newly added tokens only
  ttl     whether a gap longer than the short tier still hits the long tier.
          Wait well past the window: 330s against a nominal 300s tier leaves
          only 30s of margin, inside any lazy-eviction tolerance. The control
          case is what separates the tier from the provider's default.

Output: one JSON file under `.dev/verify/` and one verdict line. Exit 0 when
every requested scenario got an answer (including a refusal), 1 on a bad
invocation, 2 when a scenario could not be attempted.
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime

# A prefix has to reach the provider's minimum cacheable size. Kept as prose so
# it tokenizes like the prompts this server sends, not like a repeated byte.
LINES = (
    "The prefix cache is a token prefix of the whole request, quantized by the provider.",
    "A stable instruction stays at the front; the facts of this turn go at the end.",
    "A request that repeats the same bytes pays the cached rate for the reused part.",
    "A write is charged once, when those bytes first enter the cache.",
    "A hit renews a renewing tier and does not move the deadline of a leased one.",
    "Storage is billed per natural hour, and a partial hour counts as a full one.",
    "A tier is only worth its extra write when enough later requests fall inside it.",
    "What changes on every turn must be emitted as late as the stable part allows.",
)


def prefix(blocks: int, salt: str = "") -> str:
    """A stable prefix of roughly `blocks * 128` tokens."""
    lines = [f"probe {salt}".strip()]
    for block in range(blocks):
        for line in LINES:
            lines.append(f"{block:03d} {line}")
    return "\n".join(lines)


DRY_RUN = False


def call(base: str, key: str, path: str, body: dict, timeout: float) -> dict:
    """One request and everything worth keeping about its answer."""
    if DRY_RUN:
        return {
            "status": None,
            "error": "dry run: not sent",
            "body_keys": sorted(body.keys()),
            "elapsed_ms": 0,
        }
    started = time.monotonic()
    request = urllib.request.Request(
        base.rstrip("/") + path,
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as answer:
            raw = answer.read().decode("utf-8", "replace")
            status = answer.status
    except urllib.error.HTTPError as error:
        raw = error.read().decode("utf-8", "replace")
        status = error.code
    except Exception as error:  # a refused route is a measurement, not a crash
        return {
            "status": None,
            "error": f"{type(error).__name__}: {error}",
            "elapsed_ms": int((time.monotonic() - started) * 1000),
        }
    try:
        parsed = json.loads(raw)
    except ValueError:
        parsed = {"_unparsed": raw[:400]}
    return {
        "status": status,
        "usage": parsed.get("usage"),
        "error": parsed.get("error"),
        "body_keys": sorted(parsed.keys()),
        "elapsed_ms": int((time.monotonic() - started) * 1000),
    }


def cache_fields(usage) -> dict:
    """Every field whose name mentions a cache, at any nesting depth."""
    found = {}

    def walk(value, path):
        if isinstance(value, dict):
            for name, inner in value.items():
                walk(inner, f"{path}.{name}" if path else name)
        elif "cache" in path.lower() or "cached" in path.lower():
            found[path] = value

    walk(usage or {}, "")
    return found


def chat_body(model: str, system: str, directive, max_tokens: int) -> dict:
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": "Reply with the single word: ok"},
        ],
        "max_tokens": max_tokens,
        "stream": False,
    }
    if directive is not None:
        body["prompt_cache_options"] = directive
    return body


def responses_body(model: str, system: str, directive, max_tokens: int) -> dict:
    body = {
        "model": model,
        "instructions": system,
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "Reply ok."}]}],
        "max_output_tokens": max_tokens,
        "stream": False,
        "store": False,
    }
    if directive is not None:
        body["prompt_cache_options"] = directive
    return body


def brief(answer: dict) -> dict:
    """The row a human reads: status, cache fields, and nothing else."""
    return {
        "status": answer.get("status"),
        "error": answer.get("error"),
        "cache": cache_fields(answer.get("usage")),
        "usage": answer.get("usage"),
        "body_keys": answer.get("body_keys"),
        "elapsed_ms": answer.get("elapsed_ms"),
    }


def scenario_shape(base: str, key: str, model: str, timeout: float) -> dict:
    """Which surface works here, and is a cache directive acknowledged."""
    directive = {"mode": "implicit", "ttl": "1h"}
    out = {}
    for name, path, build in (
        ("chat", "/chat/completions", chat_body),
        ("responses", "/responses", responses_body),
    ):
        out[name] = {}
        # A different salt per case: a repeat of the same prefix would hit the
        # cache on its own, which would look like the directive did something.
        for label, option in (("no_directive", None), ("with_directive", directive)):
            salted = prefix(2, f"shape-{name}-{label}")
            answer = call(base, key, path, build(model, salted, option, 8), timeout)
            out[name][label] = brief(answer)
    return out


def scenario_write(base: str, key: str, model: str, timeout: float) -> dict:
    """A write after a warm prefix: only the extension, or the whole prefix again."""
    stable = prefix(10, "write")
    extension = prefix(2, "extension")
    directive = {"mode": "implicit", "ttl": "1h"}
    steps = (
        ("cold", stable),
        ("repeat", stable),
        ("extended", stable + "\n" + extension),
        ("extended_repeat", stable + "\n" + extension),
    )
    # Only the Responses surface reports `input_tokens_details.cache_write_tokens`;
    # Chat Completions reports the hit but not the write, so it cannot answer this.
    return {
        label: brief(call(base, key, "/responses",
                          responses_body(model, text, directive, 8), timeout))
        for label, text in steps
    }


def scenario_ttl(base: str, key: str, model: str, timeout: float, gap: int) -> dict:
    """Does the long tier still answer a hit after the short tier's window."""
    if gap <= 0:
        return {"skipped": "pass --ttl-wait <seconds> to measure"}
    # Every case is written first and then read after one shared wait. The
    # control sends no directive at all: it separates "the ttl field changed the
    # lifetime" from "the default tier already outlives the short one".
    cases = [
        ("5m", {"mode": "implicit", "ttl": "5m"}, prefix(10, "ttl-5m")),
        ("1h", {"mode": "implicit", "ttl": "1h"}, prefix(10, "ttl-1h")),
        ("control", None, prefix(10, "ttl-control")),
    ]
    cold = {
        label: call(base, key, "/responses",
                    responses_body(model, system, directive, 8), timeout)
        for label, directive, system in cases
    }
    if not DRY_RUN:
        time.sleep(gap)
    out = {}
    for label, directive, system in cases:
        second = call(base, key, "/responses",
                      responses_body(model, system, directive, 8), timeout)
        out[label] = {"cold": brief(cold[label]), f"after_{gap}s": brief(second)}
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get(
        "CACHE_PROBE_BASE_URL", "https://api.kimi.com/coding/v1"))
    parser.add_argument("--model", default="kimi-for-coding")
    parser.add_argument("--key-env", default="KIMI_CODING_API_KEY")
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--ttl-wait", type=int, default=0,
                        help="seconds to wait before the repeated request; 0 skips the ttl scenario")
    parser.add_argument("--only", choices=("shape", "write", "ttl"),
                        help="run just one scenario")
    parser.add_argument("--output", default="", help="override the .dev/verify JSON path")
    parser.add_argument("--dry-run", action="store_true",
                        help="show the request shapes without sending anything")
    args = parser.parse_args()

    global DRY_RUN
    DRY_RUN = args.dry_run
    key = os.environ.get(args.key_env)
    if not key and not args.dry_run:
        print(f"no credential: set {args.key_env}", file=sys.stderr)
        return 1
    key = key or ""

    started = time.monotonic()
    scenarios = {}
    if args.only in (None, "shape"):
        scenarios["shape"] = scenario_shape(args.base_url, key, args.model, args.timeout)
    if args.only in (None, "write"):
        scenarios["write"] = scenario_write(args.base_url, key, args.model, args.timeout)
    if args.only in (None, "ttl"):
        scenarios["ttl"] = scenario_ttl(args.base_url, key, args.model,
                                        args.timeout, args.ttl_wait)

    if DRY_RUN:
        print(json.dumps(scenarios, ensure_ascii=False, indent=2))
        return 0

    output = args.output or (
        ".dev/verify/cache-probe-"
        + datetime.now().strftime("%Y%m%d-%H%M%S") + ".json"
    )
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", output)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(
            {"at": datetime.now().isoformat(), "base_url": args.base_url,
             "model": args.model, "ttl_wait": args.ttl_wait,
             "elapsed_ms": int((time.monotonic() - started) * 1000),
             "scenarios": scenarios},
            handle, ensure_ascii=False, indent=2,
        )

    def count_answers(value):
        if isinstance(value, dict):
            if "status" in value:
                return 1
            return sum(count_answers(inner) for inner in value.values())
        return 0

    answered = count_answers(scenarios)
    attempted = len(scenarios)
    print(f"OBSERVED · {attempted} scenarios · {answered} answers · "
          f"{int((time.monotonic() - started) * 1000)}ms · {output}")
    return 0 if answered else 2


if __name__ == "__main__":
    sys.exit(main())
