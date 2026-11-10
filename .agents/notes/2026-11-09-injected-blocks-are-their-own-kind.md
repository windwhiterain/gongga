# An injected block is not the operator's line

User's ruling, 2026-11-09: a prompt the harness injects renders as the
operator's message; it should be a block of its own, with its own symbol and
colour. And a channel's notice should be identical to the `Result` a
`new`/`check` call answers with.

## Colours, as the user set them

| line | ink |
| --- | --- |
| user message | pink (`LightMagenta`) |
| harness injection | cyan (`LightCyan`), mark `\u{21b3}` |
| cmd | purple (`Magenta`) |
| cmd result | green (`Result`) |
| tool call | yellow (unchanged) |

## What makes a line an injection

It was stored as `text` with a user role, which is everything the model must keep
seeing. So the row kind is what changes: `inject`, written through
`append_user_conn`'s `kind` argument for a pending inject task and by
`settle_handover` for the lesson. The provider content is untouched, the wire says
`ResponsePart::Inject`, and the client draws `LineKind::Inject` -- its own block,
never folded.

## A channel's notice

It used to be a sentence plus a 500-char preview. It is now `Outcome::result`: the
same `{id, output, time, matched, exit}` the tool answers with, capped by the same
`spill::cap_output`, and it advances the cursor because it is a read. The row goes in
as `inject`, so it draws as the harness block rather than as the operator's.
