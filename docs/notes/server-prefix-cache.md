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
records `touched`, the moment the provider last answered a request carrying that chain,
and confirming a chain refreshes every node on it, so `touched` never decreases towards
the root: a node is stale exactly when its own prefix expired.

`touched` used to be written by `plan`, which made it a claim about a choice rather than
about a request: a 400, a dead socket, a cancelled turn all marked a chain warm that the
provider never held. It is now written by `confirm`, called from the one place that knows
the provider answered -- `LlmEvent::Completed` in `agent`, with the spellings the request
emitted (`Prompt::rooted`, which is the index's own order). Retries inside `llm::stream`
report a failure or a retry and only the attempt that completes counts, so a request that
took four attempts warms nothing until the fourth works.

A plan still creates nodes for entries it has never emitted, and those are born at the
current instant -- they would look warm to `expire` if the request that created them
failed. Nothing reads them until the chain is chosen again, and a chain the provider
never held is a miss rather than an error; the first request after that failure confirms
what actually went out. Making that exact would mean dropping unconfirmed nodes, which is
worth doing when something else needs the distinction.

The skill table's ordering (`tools/index_skill`) is planned the same way but has no
confirm: it is a tool result inside the transcript, so it is not a prefix the provider
holds for the *next* request's sake. Its nodes now expire with their tier and the table
falls back to the caller's order -- no behaviour change, because the row was already
written and replayed verbatim.

`expire` runs first, before anything reads a node index. A stale node takes its whole
subtree with it, because a provider's cache is a prefix -- if the front of a chain is
gone, the rest of it cannot be served from cache either, and keeping it would let a plan
choose a chain whose first half is a miss.

`ttl_ms` is the roster's number for the model in use: `[[provider]] cache_ttl_secs` is a
fact about that provider -- which mode's cache, and how each vendor bills it, is
`docs/reference/prefix-cache.md` -- so a session on a five-minute cache stops planning for warmth
five minutes after it was last sent, while the session beside it on a three-day cache
does not. It used to be one number for the whole machine, which meant either believing
nothing (the shipped `0`, every provider planned for as warm forever) or believing one
provider's lifetime about all of them.

`ttl_ms == 0` never expires, and that is what a provider stating no lifetime leaves it
at: a lifetime nobody stated is not one to invent, and prose in the roster ("约 5 分钟")
is not parsed into one. `[agent] cache_ttl_secs` / `GONGGA_CACHE_TTL_SECS` is the
machine's own answer and wins when it is not zero -- one lifetime for every provider, for
a machine that knows better than the roster.

## the tier in force, and the one assumption under `expire`

A provider rarely has *one* cache lifetime. Kimi sells 5m and 1h; Volcengine sells an
explicit `expire_at` from one hour to seven days beside an unpromised implicit cache;
Alibaba sells a five-minute explicit cache beside an implicit one with no stated expiry.
So the roster's number is not "how long a prefix lives" but "which tier a request that
names no cache mode lands in, and how long that one lives" -- `cache_ttl_tier` names it
and `cache_ttl_secs` is its length.

That is the tier this server buys, because **it sends no cache directive at all**: no
`cache_control`, no `expire_at`, no `prompt_cache_retention`. The choice is therefore the
provider's default, and planning for the longest tier it offers instead would be a plan
that finds cold chains and re-sends them at full price without saying so.

`expire` counts from `touched`, refreshed every time a chain is sent, which is right
exactly for the tiers that renew on a hit -- Kimi, MiniMax and Alibaba's explicit cache
all state that. Volcengine's explicit tier is the counterexample: `expire_at` is a
deadline from the write, and continuing to use it does not move it. No in-force tier is
of that kind today; a tier that is would need the deadline recorded when the chain was
first written, not when it was last sent.

## priced from the roster: Rates

The cost model used to be a constant of this module (`MISS_SCALE = 64`). It is now
the roster's own numbers: `[[provider.model]]` may carry `[provider.model.price]`,
and `Price::miss_ratio()` turns `input / cache_read` into the factor one cached token
is worth. Only that ratio is used -- a currency is never converted and a unit is never
assumed, so `1.00/0.10 CNY per million` and `0.15/0.015 USD` are the same 10.

`Rates` also carries `cache_invalid_rate`, from `[agent]` or
`GONGGA_CACHE_INVALID_RATE`, and the lifetime above. The probability is the machine's
hyperparameter -- it is a claim about how often any provider drops a prefix, not a
measurement -- and it is folded in as an expectation: a cache correct `(1 - p)` of the
time is worth `(1 - p)` of its discount, so

    scale = ratio / ((1 - p) + p * ratio)

At `p = 0` the scale is the roster's ratio; at `p = 1` it is `1.0`, which says planning
for warmth buys nothing and the caller's own order stands. That is the whole knob: no
other number in this module decides anything.

When the roster states no usable pair, the scale falls back to
[`DEFAULT_MISS_RATIO`] -- a stand-in, because the alternative is worse than a guess: a
plan made at `1.0` never chooses a warm chain, so every call would append a node and
the layout would grow a branch per request while warming nothing.

## a measured `invalid` replaces the hyperparameter

`cache_invalid_rate` was the only claim in `Rates` that came from this machine
rather than the roster: a guess about how often any provider drops a prefix.
`Agent::rates()` now asks `Store::cache_hit_posterior(model)` first, averaging
`1 - hit_ratio` over this home's own `usage` event rows. A row counts only when it
has both a cached count and a non-zero expected prefix -- the previous answer's
`input_tokens`, which is the largest prefix this one could hit because the
transcript only grows and the prompt row is replayed verbatim. With no such row the
configured value still answers, so a new home's first request is priced exactly as
before. The formula above is unchanged; only the source of `p` moved.

Those rows also carry `uncached`, `gap_ms`, `cache_tier` and `cache_write`. The
per-request tier choice in `cache_tier::choose` reads the gaps to price a window,
and reads `cache_tier` back with `hit_ratio` to replace the roster's stated window
with an observed `P(hit | tier, gap)` once this home has rows for that bucket.
Until then a tier's window is the roster's number used as a hard step, never a
grace period invented here. See `docs/notes/server-cache-tier.md`.

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

The identity is a digest of the text, so two entries are the same item exactly when
their bytes hash alike. The bytes are kept beside it as well, and that is what the next
section is about: a patch is measured against the text the provider was actually sent.

## tolerance: keep the node, append the change

An edited file used to cost the whole index. New bytes, new item, the old item dropped
with every chain through it -- so the request re-sent the entry *and* everything planned
after it. What that throws away is the prefix, not the edit: measured against a real
provider, an edit at one point of the instructions keeps everything before that point
(inserting a node between two others kept 5120-5504 of ~10.6k input tokens), while one
line appended at the end kept 98.6%. Where a change sits decides what it costs, and the
end is the cheapest place there is.

So an entry whose text changed is measured before it is replaced. `diff::patch` says how
many of its lines the change touches; under the caller's tolerance the item *stays* --
same identity, same nodes, so every chain that carried it is still a candidate -- and the
plan answers with the text it already holds plus a patch to append after the whole index.
The request keeps its prefix and pays for the change alone.

Three things make that safe:

- **The patch is verified.** `diff::patch` applies its own output back to the old text and
  returns nothing when it does not reproduce the new one. A model reading a patch that
  does not apply would act on text this machine does not hold: worse than a cache miss.
- **The text is what was sent, not the file's last version.** An item keeps the text the
  provider holds, so a second edit is a diff from *that*. Two small edits to one file
  therefore append one patch that says both, and the entry above is stale by exactly what
  the patch says -- never by what two separate patches would each say.
- **The prose that introduces it is a template, and without it the tolerance is off.**
  `Agent::index_tolerance` asks for `reference_revision` before allowing a patch, so a
  home pinned before that template existed keeps indexing and re-sending exactly as it
  did. Off is also the answer for a tolerance of `0`.

What it costs is in the row: an item keeps its text rather than only a digest, because
the next plan diffs against it. A skill body is the largest thing in a `prefix_layout`
row, and carrying it is the price of being able to say what changed instead of saying
everything again. A row written before this -- an item with a digest and no text -- does
not parse, so every session plans cold once after the upgrade: one re-billed prefix, and
the honest way to change what an item is.

Two readings of "no change" are worth knowing. A text that differs only in its final
newline measures zero changed lines, so the entry is kept and nothing is appended: to a
reader the two are the same text. And a change of exactly the tolerance -- `0.2` of a
five-line entry is one line -- is over it: the boundary falls on the side of re-sending.

Only the prompt index asks for this. The skill table passes `None`: a row is json, and a
diff of json is not something a reader reads.

## shared, plan: one layout per home, on disk

A provider caches a prefix per endpoint, not per session and not per process. So the
thing that remembers orderings is the home's database, not a field of anything that
dies: one row in `prefix_layout`, read once into the process's mutex and written back
after every plan. A restart is a new process asking the same endpoint for the same
bytes, and it has to plan from what the last run learnt or its first request is cold.

`plan(store, wanted, rates, tolerance)` is the whole interface, deliberately: a caller
hands over keys with their text and sizes and gets an order back -- each entry with the
text to send under it, plus the patches that catch up what changed. It knows nothing
about chains, and nothing about which of its entries were kept as they were. `index_skill` and
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

The good reading: the index is worth its bytes, because a turn that follows a warm one
is billed almost entirely at the cache rate (¥0.10/1M against ¥1.00/1M on this roster)
-- the index, the prose and the schemas are re-used rather than re-sent.

### the 1.2% was a miss, not the price of the change

That third row was read as "the minute in the prompt took the whole prefix", and two
designs were made to follow from it. A later probe, one variable at a time and repeated,
says otherwise:

- **A prompt the provider has not seen answers with 128-189 cached**, whatever changed and
  wherever: 5 of 13 first-time prompts in one run. So the first reading after an edit
  measures the lookup, not the edit. The reading that means something is the same request
  sent again -- and an unchanged prompt was served from cache 20 times out of 20.
- **A change inside the `instructions` string keeps everything before it.** Inserting a
  node between two others kept 5120-5504 cached of ~10.6k input: the prose and the node
  before the insertion, and nothing after it. One line appended at its end kept 98.6%.
- **The reused amount is always a multiple of 128 tokens**: the cache is a token prefix of
  the request, quantized to the provider's own block.
- **The same content in two shapes, three repeats each**: one `instructions` string 97.7%,
  one input item per node 94.5%. Splitting into items costs about 280 tokens at steady
  state and saves nothing.

So the cache is a prefix of the whole request, not a sequence of units: where a change
sits decides how much follows it, never which item it is in. The rule to carry is the one
about position -- **what changes every turn goes as late as the stable part allows** --
and the first reading after any edit is not evidence about what the edit cost.

What the tail's move out of `instructions` was therefore worth is not cache: a value at
the end of the string keeps 98.6% either way, measured. Its reasons are elsewhere and they
stand -- a message that follows the transcript is read as a fresh injection, and a stamped
card is not re-rendered at all. See `docs/notes/server-body.md`.

## the index is derived, not stored

`Layout::index` maps `(key, digest)` to an item and `Layout::named` maps `(purpose, key)`
to the item that name resolves to now; both are `#[serde(skip)]`, because JSON has no
tuple key and both are lookups over `items` rather than facts of their own. So the store
holds the list and the chains, and `reindex` rebuilds the pair -- `store::prefix_layout`
calls it, so no caller can hold a layout whose maps disagree with its items. That is also
why `forget_unwanted` rebuilds them after renumbering: one writer for one map, and no way
for the two to disagree.
