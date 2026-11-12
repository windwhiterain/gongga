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

## Written once

The head is written once, when the session first asks for it, and only read
afterwards. `Store::write_head_once` refuses a second write, so no code path -- a
setting, a skill, a tool, a command -- can append another schema. The row is the
record; `store::tool_set` reads it back and a request sends those bytes.

The schema is the full set for the life of the session. A remote tool and the
`remote` argument are always declared, and the `live_compact` schema is offered for
as long as compaction is enabled -- not only once an origin opens. Which of them a
call may actually reach is announced at the tail, never by re-rendering this head.

This is the append-only rule made structural. The schema used to be re-stamped when
an origin opened or a session moved, and each re-stamp was a second set of bytes for
a prefix the provider had already cached -- measured at 85k-98k tokens cold-started
at one explore -> work boundary, more than every summary of a run put together.
Settings are not context: `set_session` may change the model, the peer or the mode
freely, and none of them re-renders these bytes.

## Cost

The schema sits in every request (a few hundred cached tokens) and the head never moves. The measured effect is in
`.dev/verify/<sha>-live-compact-head.json` and `.dev/verify/<sha>-live-compact-ratio.json`.
