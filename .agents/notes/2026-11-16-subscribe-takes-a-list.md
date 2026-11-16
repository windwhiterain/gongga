# `subscribe` takes a list, and a match fills it (2026-11-16)

## What changed

SPEC's `subscribe(id, pattern, commands)` ran one command whose arguments were frozen when
it was armed. Two things in `ship/` existed only because of that freeze:

- `ask` minted a channel, started a shell in it, and ran `answer.py` there to read the
  mentor's transcript and send the answer back;
- the `subagent` notification was a ping (`"<child> finished its turn"`) that carried
  nothing, and the skill said so on purpose.

Now `subscribe(pattern, cmds)` runs the commands in order and **stops at the first
refusal**, and a string argument may name a field of the matched event: `{event.answer}`.
Both workarounds are gone, and the subagent's notification carries what the child said.

## Decisions, and why

- **The payload is lifted.** A log row is a `kind` plus free-form fields, so `turn_end`'s
  answer is `{event.answer}`, not `{event.payload.answer}`. `name` and `session` are added
  to every event, so an action need not know which kind it watches.
- **Only `{event.` is a reference.** Other braces and words reach the command as written,
  which is what makes it safe on message prose. An unterminated `{event.` is text.
- **A missing field is a refusal**, listing the fields that exist. The alternative is a
  message that quietly reads `{event.answer}` where the answer should be.
- **Sequence, not batch.** The first refusal stops the rest; the `subscription` row says
  how far it got (`ran`).
- **The answer rides `turn_end` whole.** `ANSWER_LIMIT` is gone: a notice that carries a
  head sends its reader back for the rest, which is one round trip per notification.
- **Old rows still read.** A row armed before this holds one command object; `Action::of`
  reads it as a list of one. The *call's* spelling is not kept -- the argument schema makes
  `cmds` required, so a `cmd` fallback in the handler could never run and was removed.

## Measured

`.dev/verify/7939f34a-subscribe-list.json`: two commands ran in order (`ran: 2`),
`{event.answer}` arrived as the mentor's whole answer, the bad path was refused with the
field list, and `ask` returned in 0.186 s.

## What is still red, and not from this change

`scripts/cmd_selftest.sh` has five channel checks failing in this environment; the
subscription checks in the same run pass, and `exec` as a command works against the live
server. There is no earlier record of that selftest in `.dev/verify`, so its baseline is
unknown -- somebody should find it rather than assume it was ever green.
