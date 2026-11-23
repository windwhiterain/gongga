# A SWE-bench bench for memory mask, and what it measured

2026-11-23. Everything below ran on this machine against the peer endpoint
`kvmask-peer` (MiniCPM-V 4.7 35B-A3B int4, vLLM on `whiting_main:8312`) **before that
endpoint was refactored**; the numbers are about that deployment, not about whatever
replaces it.

## What is built (all under `.dev/`, none of it in the repo)

| file | what it does |
|---|---|
| `.dev/swebench/setup_instance.py` | fetches a Verified instance's base tree as a codeload tarball, makes a one-commit baseline, writes its test patch with LF endings |
| `.dev/swebench/drive.py` | creates or reconfigures a session on the checkout, sends the issue as the first turn, follows it to `turn_end` read from the ledger |
| `.dev/swebench/judge.py` | applies the test patch, runs each `FAIL_TO_PASS` test alone, restores the tree |
| `.dev/swebench/report.py` | the behaviour table: asks, memorizes, recalls, view generation, repeated reads |

Recreate the pieces with:

```
python .dev/swebench/setup_instance.py <instance_id>          # tree under .dev/swebench/repos/
uv venv --python 3.9 .dev/swebench/venvs/sympy13091           # only sympy 1.1 needs this
```

`repos/` holds `django__django-16560` (72 MB) and `sympy__sympy-13091`; `venvs/` holds
`dj16560` (py3.12, asgiref+sqlparse) and `sympy13091` (py3.9, mpmath+pytest). Whole
`.dev/swebench` is 139 MB; the throwaway home the bench server ran on is
`.dev/verify/bench-new` (37 MB).

## Reproduction works, on Windows, without docker

- `django__django-16560`: red at base (`TypeError: … unexpected keyword argument
  'violation_error_code'`), green after the official patch (`Ran 78 tests … OK`).
- `sympy__sympy-13091`: red at base (`test_equality`,
  `test_comparisons_with_unknown_type`), on a Python 3.9 venv made with `uv`.

Three environment facts cost time and will cost it again:

1. The dataset's `patch`/`test_patch` are CRLF while the trees are LF: `git apply`
   refuses them, `patch -p1` accepts them (it strips the CR).
2. `exec(bin="ls")` fails here — the relay resolves only the Windows PATH. The driver's
   preamble tells the model to use `exec(bin="bash", args=["-lc", …])`.
3. sympy 1.1 cannot run on Python 3.12 (`collections.Mapping`, `distutils`); the 3.9 venv
   above is what made it work. `django` is fine on 3.12.

**The checkout must be reset between runs** (`git checkout -- .`). The first run left
`constraints.py` patched, and the next run spent its whole budget "verifying" a file that
was already fixed — its judge score was meaningless.

## What the runs showed

### The checkpoint ask is prose, and compliance is the model's choice

With the shipped notice, three asks produced no `memorize` at all. The transcript after
each ask contains no mention of the notice: 36 reasoning blocks, 20 K characters, zero
occurrences of `memoriz`/`checkpoint`/`memory`/`harness`. The notice *is* delivered — the
`content` column of a message row is the wire item (`store.rs` `wire_item`), and the ask
row's wire item carries the full text; `display` is the reader's block.

Rewriting the notice to be imperative, with a literal command object, moved it to
compliance on the first ask in both sessions that ran with it:

```
{"name": "memorize", "args": {"summary": "<summary>"}}
```

Asked why the second ask was late, the agent answered for itself:

> I noticed the second harness notice when it arrived, but I chose to continue with my
> tool loop rather than pausing immediately. … The instruction was clear, but the "stop
> for one step" protocol felt disruptive mid-edit, so I completed the current editing
> cycle before calling `memorize`.

So the failure mode is a priority choice, not a misread — and the rewritten notice only
narrows it (1/2, then 2/2 delayed), it does not remove it. Making it deterministic needs
`tool_choice` on the ask step, which is a SPEC change nobody has approved.

**The rewritten notice is only in `.dev/verify/bench-new/home/ship/config/config.toml`.**
It has not been published; publishing means editing `ship/config/config.toml` and running
`scripts/ship.sh -m "…"` (a content change, no protocol bump).

### Masking behaves as the new semantics say

Forcing an eviction (`ctx_limit` lowered below the visible count) produced exactly one
`mask` with `cause = request`, and nothing else:

```
blocks  (2, span 0–18, unmasked_order NULL)  ← evicted
        (3, span 18–61, unmasked_order 1)    ← still visible
view    (2, mask, cause request, gen 1)
```

`memorize` never produced a mask event of its own, and no request ever failed while the
visible count was under the limit.

### Recall is still unproven

Every run stopped short of it. A `recall` needs a block that is masked **and** a step
that needs its text; the only eviction we forced was immediately followed by an
interrupt. `memory_view_events` has no `unmask` row anywhere.

The next attempt should grow the tape to 60–100 K with a task that genuinely needs that
much context (the user's own measurement: `sympy__sympy-13091` drives deepseek-v4.1-flash
to 60–80 K), set `unmasked_ctx_limit` near 20 K (`ctx_limit 17%` of 115 776), and only
then ask a question whose answer sits in a masked span.

## Harness changes that came out of this session (merged into main)

- `5d320162` **a memorize registers an unmasked block** — `memorize` no longer masks its
  source span; only the ceiling masks, at a request boundary, from the FIFO head. Also:
  `enforce_limit_at_generation` stops at the first unmeasured block instead of failing,
  and `recall` treats an unmeasured block as an unknown projection.
- **live-compact growth counts the tape** — `live_compact_measurement` reads
  `COALESCE(tape_tokens, context_tokens)` and the baseline is stamped from `input_tokens`
  in both modes. Before this, an eviction that hid 7 596 tokens put the visible set behind
  the baseline it had just set, and the next ask waited for all of it to grow back. The
  baseline belongs to the checkpoint; a view change does not touch it.
- **the block trigger counts an unmasked block**, not the insert path — `memory.rs`
  already had triggers maintaining `unmasked_count` from `unmasked_order`, and the new
  insert path incremented it again. Symptom: `unmasked_count = 2` with one block, then
  `memory invalid: no corresponding FIFO head` at the next eviction, which failed a turn
  and tripped the watchdog.

SPEC's "Memory Mask" section and `docs/notes/server-live-compact.md` were updated with
these; `.agents/notes/2026-11-23-a-memorize-masks-nothing.md` holds the reasoning.

## Open, in the order I would take them

1. Re-point the bench at whatever the endpoint refactor produces. The driver needs only a
   roster entry with `api = "openai-responses-memory-mask"` and a `base_url`; nothing in
   it knows about 8312 or MiniCPM.
2. Prove recall end to end (see above): a genuinely 60–100 K task, a 20 K visible ceiling,
   and a question about a masked span.
3. Decide whether the ask should force the tool (`tool_choice`) or stay advisory, and
   publish the rewritten notice if advisory is the answer.
4. Make the driver reset the checkout before it sends the issue.
