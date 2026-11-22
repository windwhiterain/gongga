# crates/server/src/transcript.rs

## transcript.rs

A walk over one session's sendable rows, on a connection of its own. The store
holds the shared connection; a request reads from this one while it is writing
to the network, and the shared connection has to stay free for the turn waiting
on it.

What comes back is the stored item, byte for byte. The request's shape was
decided when the row was appended, so a walk cannot disagree with an earlier
request about what a row is: rewriting a row would rewrite bytes the provider
had already cached, and each such rewrite cost the whole transcript.

`upto_seq` is fixed when the walk is made, so every row it returns was stored
before the request began. That is what lets a retry replay one window: the
window is the same set of rows, not a fresh reading of a transcript that moved
underneath it. Each yielded row also carries its database `seq`; the memory-mask
Responses body uses it for `gongga_seq`, without asking the adapter to infer seq
from wire order (outputs of one step can follow later calls).

Only checkpoints whose `kind` is `cut` create the live-compact projection. A
`mask` checkpoint leaves every sendable row in the walk; its visibility lives in
the request's memory-view snapshot instead.

The pictures ride in the row's `display`, which is what the row recorded when it
was written. An item names a copy, and the copy is read when a request is built
-- the provider's file is resolved then, and never stored in the transcript.

## A step's outputs follow its calls

The one thing the walk decides for itself is order, and only inside a step. A step
with more than one call is stored as the calls ran -- `call, output, call, output`,
because the agent executes each call inside the stream and answers it before the
next one arrives. A request cannot carry that: the items are one assistant turn,
and a provider reads `call, output, call` as two turns, the second having thought
nothing.

Measured on deepseek's Responses face, same key, same tools, same rows:

```
stored order  1,4,5,6,7,8,9  -> 400  The `reasoning_text` in the thinking mode must be passed back to the API.
walk order    1,4,5,6,8,7,9  -> 200
```

So the walk holds an output whose step still has a call to make, and releases it
when that step is over. Nothing is dropped or rewritten: every stored row still
comes back exactly once, only later. The refusal it prevents used to be answered
by a `thinking_mark` -- a fabricated user row saying a step was written without
thinking, which was never true: the step thought, and the harness reordered it.
