# `crates/server/src/prefix_cache.rs`

## module doc

A provider caches the prefix of every request it served, and bills the part it can
reuse at a fraction of a fresh token. The cache is keyed by the bytes, so the only
thing that matters about an ordering is *how far from the front it still matches
something already sent*. Two calls that emit the same set in different orders cost
each other the whole tail after the first difference.

That is the problem this module solves, and it is not this repository's problem
first: it is liana's, from that project's `src/memory.rs` (`Manager`, `Node`,
`find`, `messages`), where the thing being ordered was which memories to put in a
turn. The algorithm is a forest of chains: every ordering ever emitted is a path
from a root to a node, a node's size is how much its whole chain carries, and
picking an order is picking the cheapest node plus appending what is new. Ported
here with the same shape and the same cost model, because the problem is the same
one: some rows of text, some orderings already on the wire.

`v1` (liana `15a0978`) is where the ported shape came from -- `master` carries an older `find` with
the same cost walk but a hand-maintained node size and a `remove_memory` this port
does not need. The difference that mattered: a size is re-derived, not patched.

## MISS_SCALE

A byte the cache still holds is counted as one unit; a byte that has to be sent
again is counted as `MISS_SCALE` of them. liana used 64 and this keeps it: it is
the port's only tuning knob, and what it encodes is "a hit is a fraction of a
miss, so a chain that covers one more row wins unless it is 64 times bigger". It
is not a limit on anything and it is not a measurement -- the provider's real
ratio belongs to the provider.

## Item / Node

An item is a row that can be emitted: a key that survives renames of the field it
lives in, the size it costs today, and every node whose chain ends at it. A node
knows its parent, its children, and the sum of sizes from the root to itself.

Two ways of representing one ordering is what makes the walk possible: to ask
"does this chain already carry that item", the answer is "is this node in that
item's subtree", because a subtree below an item's node is exactly the chains that
put the item at or before that point.

## plan

The cheapest chain, then the rest.

The floor is what it would cost to send everything with nothing cached: the sum of
the wanted sizes times `MISS_SCALE`. A chain that cannot beat that is not worth
keeping, which is how an item that grew past its chain gets a new root instead of
dragging a worse prefix along forever.

Choosing is a cost walk, not a greedy prefix match: each chain starts at its own
size, and each wanted item not on it adds its size times `MISS_SCALE`. The chain
that ends up cheapest is the one that covers the expensive items, which is not
always the one that covers the most items -- and covering a cheap one is worth
less than covering an expensive one, which is the whole reason sizes are tracked
at all rather than just positions.

Two invariants are the port's own, both of them departures:

- A key absent from `wanted` is never emitted, however warm its chain is. liana
  walked a chain and emitted every memory on it, because a memory that fell out of
  one turn's selection was still part of the same conversation. A skill can be
  uninstalled, and a table that revives it is a lie about what the machine holds.
  So a chain carrying an item outside `wanted` is not a candidate at all, which is
  what `cold_chains` returning `Option` says.
- The plan is a permutation of `wanted`: what is new goes after what is warm, in
  the order the caller gave, and nothing else is added.

## recost

An item's size is measured each time it is declared, and a size that changed
invalidates every node above it. Re-deriving is one pass in node order, which works
because a parent is always declared before its child: the size read for the parent
is already this pass's answer. liana's opposite choice -- keep `remove_memory`
around and patch the sizes it touched -- answers a question this port does not ask:
a row of a skill table is re-measured, never deleted.

## one layout per session

A provider caches the prefix of a *conversation*, so what session A emitted is not warm
for session B -- and a chain only A can use must not be chosen for B. So the layout is
keyed by session: `prefix_layout` is one row per session name, the process keeps one
decoded `Layout` per session in a map, and `plan` takes the name it is planning for.

That reversed an earlier decision in this file, and the reversal is the user's: it is
also why the shared version was less useful than it looked. The system prompt now ends
with a tail naming the session (`dynamic_tail`), so two sessions' prefixes differ before
the transcript even starts. A shared layout would have been an index of chains that
nothing could ever hit, growing a branch per session and paying a scan for each.

`del_session` deletes that session's row: a new session of the same name is a new
conversation, and must not be warmed by the old one.

## Node::touched, expire

A price says what a hit is worth; a lifetime says whether there is one. Each node
records `touched`, the moment its chain was last sent, and using a chain refreshes
every node on it, so `touched` never decreases towards the root: a node is stale
exactly when its own prefix expired.

`expire` runs first, before anything reads a node index. A stale node takes its whole
subtree with it, because a provider's cache is a prefix -- if the front of a chain is
gone, the rest of it cannot be served from cache either, and keeping it would let a plan
choose a chain whose first half is a miss.

`ttl_ms == 0` never expires, and that is the default: a lifetime the provider does not
state is not one to invent. Setting it is a claim about the provider, and it is the only
place this module believes something it was told rather than measured.

## priced from the roster: Rates

The cost model used to be a constant of this module (`MISS_SCALE = 64`). It is now
the roster's own numbers: `[[provider.model]]` may carry `[provider.model.price]`,
and `Price::miss_ratio()` turns `input / cache_read` into the factor one cached token
is worth. Only that ratio is used -- a currency is never converted and a unit is never
assumed, so `1.00/0.10 CNY per million` and `0.15/0.015 USD` are the same 10.

`cache_invalid_rate` is another input, from `[agent]` or
`GONGGA_CACHE_INVALID_RATE`, and `cache_ttl_secs` / `GONGGA_CACHE_TTL_SECS` is the
last: a probability and a lifetime, both claims rather than measurements. It is the chance the provider no longer holds a prefix it
once did, and it is folded in as an expectation: a cache correct `(1 - p)` of the time
is worth `(1 - p)` of its discount, so

    scale = ratio / ((1 - p) + p * ratio)

At `p = 0` the scale is the roster's ratio; at `p = 1` it is `1.0`, which says planning
for warmth buys nothing and the caller's own order stands. That is the whole knob: no
other number in this module decides anything.

When the roster states no usable pair, the scale falls back to
[`DEFAULT_MISS_RATIO`] -- a stand-in, because the alternative is worse than a guess: a
plan made at `1.0` never chooses a warm chain, so every call would append a node and
the layout would grow a branch per request while warming nothing.

## the size is real tokens, not bytes

`Item::tokens` is what the provider counted, and it comes from the provider: every
stream's `usage.input_tokens` is recorded against the bytes the body wrote, per model
(`token_ratio`), and `Store::token_ratio` is a running measurement of tokens per
character. So an item's cost is `len * ratio`, measured for this model rather than
assumed to be four characters per token.

That measurement is not a nicety. A plan compares sizes with each other, so a constant
factor would cancel out; what it cannot cancel is a *ratio* -- and the ratio between
`@skill:self-control` and one table row is exactly what decides which chain is cheaper,
which is the decision this module exists to make.

## the identity is the content

A key is a name; the same name can come to mean other bytes. A skill body is edited, a
row's description is reworded, a file the index carries gains a paragraph -- and every
one of those leaves the name intact while the bytes the endpoint cached are gone.

liana had no such problem: a memory was append-only, so a memory id was its content.
This port has the opposite, so an item is `(key, content)` where `content` is a digest
of the bytes and `plan` takes it from the caller. Two consequences, both wanted:

- edited text is a *new* item, so the chain that carried the old bytes cannot warm it.
  Both halves are the identity, which is what `plan`'s tuple says: the key is the *raw
  spelling* (`@skill:helper`, `@file:docs/plan.md`) -- the thing that is also printed in
  the index and so is itself part of the prefix -- and the digest is the text behind it.
  A path is not special; whatever the caller prints as the entry's name is its key.
  Without this the layout would claim a prefix the endpoint cannot serve, which shows up
  as a cache miss on every request that reuses the chain -- the exact cost this module
  exists to avoid, made invisible.
- the old item is dropped by `forget_unwanted` once nothing asks for it. What stays is
  only what was just planned, so a layout does not grow a dead branch per edit.

The cost is one item per revision rather than one per name, but only until the next
plan: `forget_unwanted` runs every time, so a home that edits a skill daily holds one
version of it, not thirty.

A digest rather than the text itself: the layout is one SQLite row, and a skill body
can be larger than everything else in it put together. The digest is the identity, the
size is the cost, and the bytes stay where they live.

## shared, plan: one layout per home, on disk

A provider caches a prefix per endpoint, not per session and not per process. So the
thing that remembers orderings is the home's database, not a field of anything that
dies: one row in `prefix_layout`, read once into the process's mutex and written back
after every plan. A restart is a new process asking the same endpoint for the same
bytes, and it has to plan from what the last run learnt or its first request is cold.

`plan(store, wanted)` is the whole interface, deliberately: a caller hands over keys
with sizes and gets an order back, and knows nothing about chains. `index_skill` and
the first prompt row are the two callers so far, and they are different kinds of
thing -- a tool result and a system prompt -- which is the point of the algorithm
being generic. Callers namespace their own keys, because a key identifies bytes: the
skill table uses the `SKILL.md` path, the prompt index uses the canonical spelling
(`@skill:heir`, `@file:docs/plan.md`).

The one cost of sharing is a chain that is only partly useful to the next caller, and
that is already handled: a chain carrying an item outside `wanted` is not a candidate.
So two callers sharing one layout cannot corrupt each other's order, only fail to
share it.

## purpose: two callers, one session

The skill table and the prompt index both plan for the same session, and their keys are
different things (a `SKILL.md` path, a raw spelling). One shared map was wrong in a way
the measurement caught: each plan forgot every item it had not just been given, so the
two callers evicted each other on every call and neither kept a chain -- `skill table`
planned, then `prompt index` planned and dropped it, forever.

So an item carries the `purpose` that emitted it, `plan` takes one, and
`forget_unwanted` prunes only its own. Two callers in one conversation now coexist: they
emit different parts of the request, and a chain never crosses into another's keys.

## measured, against a real provider

MiniMax `MiniMax-M3.1-Flash`, the machine's own key, a 10.3k-token prefix (system
prose + a 355-line `self-control` body + `AGENTS.md` + tool schemas):

| request | input | cached | paid |
| --- | --- | --- | --- |
| cold first turn | 10321 | 189 | 1.8% |
| same session, next turn | 10371 | 10240 | 98.7% |
| same session, turn after the minute ticked | 10481 | 128 | 1.2% |

Two readings. The good one: the index is worth its bytes, because a turn that follows
in the same minute is billed almost entirely at the cache rate (¥0.10/1M against
¥1.00/1M on this roster) -- the whole index, the prose and the schemas are re-used
rather than re-sent.

The expensive one, and the fix: **the clock in the system prompt cost the whole prefix.**
`dynamic_tail` carries the minute, and while it was appended to `instructions` a turn that
crossed a minute boundary answered with 1.2% cached instead of 98.7% -- measured above.

That is what says a provider's cache is taken item by item rather than within one string:
the first unit that differs takes everything after it. So the tail is now its own item,
written after the transcript (`RequestBody::own_facts`), and the same turns measure:

| | cold | next turn, minute changed |
| --- | --- | --- |
| tail appended to `instructions` | 1.8% | 1.2% |
| tail as the last input item | 1.8% | 98.2%, then 99.3% |

Then the sharper fix, and the one the transcript wanted anyway: the card is stamped once,
with the session's first prompt row, and never re-rendered. What a request sent is never
rewritten, so the minute in the prompt is the session's start time -- one prompt row for
the life of the session, and the second turn now measures **99.7%**. See
`docs/notes/server-body.md`.

## the index is derived, not stored

`Layout::index` maps `(key, digest)` to an item, and it is `#[serde(skip)]`: JSON has no
tuple key, and the map is a lookup over `items` rather than a fact of its own. So the
store holds the list and the chains, and `reindex` rebuilds the map when the row is
read. That is also why `forget_unwanted` rebuilds it after renumbering: one writer for
one map, and no way for the two to disagree.

## chain_count

`#[cfg(test)]` only, and the one thing the tests cannot see from the outside: a
plan that emits the same order can still have learnt a new chain, and being able
to tell "reused" from "re-learnt" is what makes `a_repeated_set_is_emitted_unchanged`
and `a_shrunk_item_is_cheaper_than_sending_it_cold` tests of the algorithm rather
than of its output.
