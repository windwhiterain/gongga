# The tool set is a stored row

A request is `stored prompt + stored tool set + stored messages`. The tool list is
part of the context, and the context is append-only -- SPEC's `context is append
only, context that has been sent must not be change` -- so a request does not
assemble one.

## The row

`messages.kind = 'tools'`, `content` NULL like the prompt row's, `display` holding
a `store::ToolSet`: the block exactly as it goes on the wire, plus the facts it was
stamped from. `store::tool_set` reads the newest row of that kind, the way
`store::prompt` reads `kind = 'system'`.

What is stored is the rendered block, not the names of the tools: the schema text,
the parameter shapes and the presence of the `remote` argument are all part of the
request, so a request that stored only names would invent them again. `LlmClient::
wire_tools` renders it for the client's face and `Tools::stamp` renders the
definitions -- these two functions are the only renderers, and both are called only
from a stored change.

## The four facts

`store::ToolSet` records the model, the peer, whether a compaction origin has
opened, and the build. `Agent::tool_set_in_force` compares them with the session's
stored facts and appends a row only when one differs:

- the first real operator row -- the first stamp; a work-born session opens its
  compaction origin on that same row, so its first stamp already carries the
  `live_compact` schema;
- the opening of a compaction origin (plan finish, explore -> work) -- the schema
  enters and stays for the session's life;
- a move between machines -- the local/remote split changes;
- a model change -- the roster's built-in tools change, as the stored prompt
  already re-stamps;
- the build -- a build may rename or reshape a tool, so a session that outlived it
  must not keep advertising a schema this server no longer has.

A transient flag, a command, a tool or a skill cannot change a head. The
`live_compact` schema used to enter and leave the tool list per request; that is
gone, and `docs/notes/server-live-compact.md` says why.

## Cost

The schema sits in every request (a few hundred cached tokens) and the head stops
moving twice per compaction cycle. The measured effect is in
`.dev/verify/<sha>-live-compact-head.json` and `.dev/verify/<sha>-live-compact-ratio.json`.
