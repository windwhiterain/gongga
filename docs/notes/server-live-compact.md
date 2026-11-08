# Live compaction, as the server implements it

Clause 3 of the SPEC's Live Compact section is the trigger: the server asks, the
agent writes the summary, and the server commits it with its result. What follows
is how the code keeps that bargain, and the two decisions that surprised me.

## A call nobody asked for is a no-op

`append_live_compact_result` needs a durable request: the request row carries the
segment, the retained round and the generation the summary is committed against.
A `live_compact` call with no request outstanding therefore has nothing to commit
against.

It used to fail, and the failure was written into the transcript as an `ERROR`
result. That is what invites a retry loop: the model sees an error, assumes the
call was malformed, and calls again -- and the head it re-sends is paid for.

A call with no outstanding request now returns a successful result that says
plainly that no checkpoint was outstanding and that the history is untouched. It
appends no checkpoint row, bumps no context generation, resets no baseline, and
writes no message beyond that one result. Only the notice can make the server
compact; a spontaneous call cannot.

A request that *is* outstanding but belongs to an older context generation still
fails: that is not a no-op, it is a summary written against a history that has
moved, and the agent must know.

## Only a lineage that planned compacts

A compaction origin opens at one place: plan finish, the explore -> work
transition, which is the agent's own act inside its turn. A session created
directly in work never opens one, so it has no baseline, is never asked for a
summary, and never carries the `live_compact` schema, whatever it does. An origin
opened on the first operator row instead would give such a session a baseline
taken from inside turn one, which is not the boundary clause 2 names.

## A summary may cite a file

Clause 4 is a capability: a summary may carry `@file(<path>)`, and the citation
embeds that file whole into the summary, so a later turn reads the content
without opening it again. It is the agent's call, and the notice and the tool's
parameter description say what it costs rather than whether to use it: an
embedded file travels in every later request, and a summary is never a cache hit.

## The schema is stored, not offered per request

See `docs/notes/server-tools.md`: the tool list is a stored row, so the
`live_compact` schema enters when the session's compaction origin opens and stays
for its life. It is not added while a request is outstanding and withdrawn after
its commit -- that entry and exit sat inside the cached prefix and invalidated the
whole conversation at every checkpoint.
