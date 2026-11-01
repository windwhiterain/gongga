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

## chain_count

`#[cfg(test)]` only, and the one thing the tests cannot see from the outside: a
plan that emits the same order can still have learnt a new chain, and being able
to tell "reused" from "re-learnt" is what makes `a_repeated_set_is_emitted_unchanged`
and `a_shrunk_item_is_cheaper_than_sending_it_cold` tests of the algorithm rather
than of its output.
