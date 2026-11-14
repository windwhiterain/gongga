# The quota block is the providers asked last, side by side

WIP on `feature/quota-top-k`. The verdict this follows: the window's `active_quota`
was one provider -- whichever step ran last -- and the operator asked for the `k`
providers that were asked *most recently*, arranged horizontally and wrapped.

## What decides the order

The user's answer to "which providers count as recently active": **the last
request time**, not the last reading time. So the ordinal is stamped where the
request is made (`llm::stream` -> `quota::note_request`) and never where the
answer is filed: an endpoint that takes ten seconds to answer must not look like
the provider that was asked last.

`Quota` therefore carries `request: u64`, one count shared by every provider.
It is not the event's position -- quota events have none -- and the client sorts
by it rather than by arrival, so the greeting's replay puts an attaching window in
the same order a long-lived one has.

## What decides the count

The user's answer: a **fixed** k. `client::QUOTA_PROVIDERS = 3`, the block's own
number, never a budget on what the server keeps. A provider that falls out is
published again at its next step's end, so nothing has to be remembered to bring
it back.

## Where the pieces are

- `quota.rs`: `by_provider` (already there) + `requested`/`next_request`,
  `readings()` (ordered), `note_request`, and `publish` stamping what it publishes.
- `net.rs`: the greeting replays every row instead of the newest one.
- `client`: `Layout::quota` is a `Vec<QuotaRow>`; a reading replaces its own
  provider's row, the rows sort by `request`, and `truncate(QUOTA_PROVIDERS)` is
  applied last.
- `tui`: `Chrome::quota_rows` is what the block wraps into, and `quota_lines` is the
  one function the height and the drawing both call.

## The observation (the thing the unit tests cannot see)

`scripts/tui_live.py`'s stand-in roster had no `quota_query`, so no frame could
show the block at all. It now writes a reader per provider into the run directory,
and `--scenario quota-block` steps two providers in one run. Frames from
`.dev/live/20261114-125108/frames.txt`:

    100 columns:  fake · month 12% · week 90% | deepseek-official · month 75% · week 40%
     52 columns:  fake · month 12% · week 90%
                  deepseek-official · month 75% · week 40%

That is the order (asked last, drawn first), the horizontal placement, the wrap, and
two providers' own numbers -- not one row copied twice. Recorded in
`.dev/verify/a0720edb-quota-block.json`.

## What is left

- The SPEC line is written on this branch and not yet merged: merging it is the
  user's call, since SPEC.md is theirs.
- 3+ providers in one frame is not observed: the count is a `truncate`, and the
  scenario has two.
- `scripts/ship.sh --check` exits 1 for a reason outside this change: the content
  checkout in the main worktree has diverged from its `origin/main`.
