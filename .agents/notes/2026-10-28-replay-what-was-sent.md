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

## R1-R3 and R5 are done: the prompt is a row, and nothing is invented at send time

The user's rule, verbatim: **重读只重读新入库的，入库的不改**.

- `store.rs`: `Prompt { instructions, summary, cwd, stamp }` written as a `system`
  row; `Store::prompt` reads the newest one; `stamp(note)` is a hash of the note so a
  fold is noticed without rendering prose. `system` is skipped when building history
  (it *is* the prompt; sending it back would double it) and described -- not dumped --
  in `messages_between`.
- `agent.rs`: `prompt_in_force` sends the stored row and re-renders only when something
  moved: no row yet, the note's stamp changed (a fold, mid-turn included), or the turn's
  first step and the directory differs. `prompt_moved` is the pure decision, tested.
- `close_dangling_calls` runs at the turn boundary and writes the `interrupted_call`
  result as a real row, reading raw rows rather than `history_since` -- whose own
  placeholder is exactly what must not hide the gap from this write.
- Ship prose follows the new truth (`[prompts]` header, `constitution` comment): pushed
  as `f8e504c`, re-pinned here, and verified to merge cleanly into a home already
  installed at the previous pin (`updated=1`… `conflicts=[]`, pin advanced).

`scripts/replay_audit.py` is the check that makes the claim mean something: it drives a
real server against the fake, with a prose edit between turns and a fold mid-conversation,
then compares every request body with the rows. Findings on this run: 4 requests, 13 sent
items, **0 unexplained by the database**; 3 distinct `instructions` texts, 3 stored rows;
every call has a stored result; and the guard that every request went to
`http://127.0.0.1:8785/v1/responses`.

**A rule for that script, learned the hard way:** it writes its own roster with the fake's
URL and must never fall back to `~/.agents/llm.toml`. An earlier version of it did, and 6
requests went to the machine's real DeepSeek endpoint with the real key before the missing
dump file gave it away. An audit that reaches a provider spends somebody's quota; the
`url=` guard at the end is now part of its contract.

## Measured cost, again, with the boundary in place

`cached` per step in that audit: 0 (cold) → 1441 (35%, the turn after the edit) → 4007
(99%, the very next step) → 1441 again at the fold boundary. So one edit costs the
boundary step and recovers immediately, and no step pays for a file read it did not need.

## What is left

- **R4** `storable_activity` deletes `result` base64 and marks the item non-replayable
  forever; write it to `<home>/blobs/<sha256>`, keep the reference in the row,
  reassemble at replay, delete blobs with the session. This is the last way an item can
  be sent that the database does not hold -- rare (only provider-side image generation),
  but it is the same hole R5 closed.
- A directory change lands at the next turn's first step, not mid-turn: `set_session
  --cwd` and a shell that moved are noticed at the boundary. If walking into another
  project should refresh the prompt immediately, that is a second trigger to add.

## The policy question is settled

Whether a prose edit may land mid-turn was the open fork last round; the user answered
it -- 入库的不改 -- and this is that answer in code. The cost is one boundary step, and
the recovery is the step after it.

## Gates

`cargo test --workspace` 218 server tests (new: `usage_keeps_what_the_provider_cached`,
`a_prompt_row_is_sent_and_never_repeated_as_history`,
`a_dangling_call_is_closed_with_a_row_not_a_fiction`, `the_prompt_is_rebuilt_only_when_something_moved`),
`test-layout-check` 0, `comments-check` 0, and `python scripts/replay_audit.py` reports
`FAILURES: none`. Smoke servers and fakes were killed by pid; `tasklist` shows no
`gongga-server.exe` and nothing listening on the scratch ports.

