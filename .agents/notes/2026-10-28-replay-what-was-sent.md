# 完全重放：库里有的 = 发出去的（WIP）

User's question, and the answer that turned into work: 「db里的对话记录是否能完全重放上下文？否则复制回导致缓存前缀变化」. Then: 「改成完全重放」, and 「那就收进来」 for the measurement.

Two things the user's framing got right and one it did not, and the disagreement is
worth keeping on record because it decided the design:

- Right: the transcript should be the whole context, and a side table for "what was
  sent" would be a second source of truth -- the exact bug class S5 just removed.
  So the volatile pieces go **into `messages` as rows**, not into a new table.
- Right: today's `interrupted_call` is invented at read time (store.rs:457), so the
  request contains an item the database never held.
- Not right: `instructions` and `tools` are not `input` items in the Responses API.
  They are top-level fields (`{model, instructions, input[], tools[], store, stream}`),
  and there is no item shape for a tool definition. What can be done is to make the
  **rendered text** a durable row that the builder reads and puts into the top-level
  field -- single source of truth, still API-shaped. `tools` needs nothing: no
  `description()` depends on runtime state (checked, tools/mod.rs:223), so the array
  is byte-stable for the life of the process.

## R6 is done: the cache is now measurable

- `Usage` gained the nested `input_tokens_details.cached_tokens` and a
  `cached_tokens()` accessor (llm.rs:643); `None` stays "the provider declined to
  say" rather than becoming a fabricated 0.
- Every completed request appends `events` kind=`usage` (model, input, cached,
  output, total) and logs `provider usage` at info. No new published command: a
  billing detail does not belong on the surface the agent is prompted on.
- `scripts/fake_llm.py` now *models* the prefix: units = [tools, instructions, each
  input item], and `cached_tokens` is the leading run identical to the previous
  request. Without that, none of this is verifiable without spending provider quota.

Measured on a scratch home, 5 steps, one sentence added to `[prompts] instructions`
before step 4 and removed before step 5:

| step | input | cached | hit |
|---|---|---|---|
| 1 | 3934 | 0 | 0% (cold) |
| 2 | 3967 | 3934 | 99% |
| 3 | 4000 | 3967 | 99% |
| 4 | 4044 | **1441** | **35%** ← one prose edit |
| 5 | 4067 | 1441 | 35% ← and it did not come back |

That is the cost of the "每步重读" rule from the templates round, in tokens: prose
sits *in front of* the transcript in the cached prefix, so an edit re-bills the whole
history behind it, and reverting the edit does not restore the hit -- the next request
has to match the request that carried it.

## What is left, in order

- **R1-R3** `instructions` and the rendered `summary_preamble` become rows (`system`,
  `summary`) written at the moment they are sent; `step()` compares the rendered text
  with the newest row and appends only when it differs. Replay becomes byte-faithful;
  live reload keeps working; and the cache break becomes *visible in the transcript*
  instead of inferred.
- **R5** stop synthesizing the closing result for an unanswered `tool_call`: write it
  durably when the call is abandoned (and on the rebuild path after a crash), so the
  array equals the rows.
- **R4** `storable_activity` deletes `result` base64 and marks the item non-replayable
  forever; write it to `<home>/blobs/<sha256>`, keep the reference in the row,
  reassemble at replay, delete blobs with the session.
- **Open, and now quantified**: whether a prose edit should be allowed to land mid-turn
  at all. 99%→35% per edit says the cheapest fix is to land template changes at the
  next fold or session boundary rather than at the next step -- which is a policy
  choice, not an implementation detail, and needs the user.

## Gates

`cargo test --workspace` 215 server tests (one new: `usage_keeps_what_the_provider_cached`),
`test-layout-check` 0, `comments-check` 0. Smoke servers and fakes killed; `tasklist`
shows no `gongga-server.exe`.
