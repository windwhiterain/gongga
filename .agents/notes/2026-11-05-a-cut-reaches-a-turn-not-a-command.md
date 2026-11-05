# A cut reaches a turn, not a command

The wait for a tool call used to belong to the `bash` body: a `select` over the command, the
session's cancellation token and a 250 ms inbox tick. `exec` and `python` wait the same way
and had no answer, so a `remote_exec` waiting out a long timeout could not be reached by
`send_message` or by `interrupt_session`. The fix puts the wait in `ToolRegistry::run`
(`waited`) and gives each tool `Tool::cut` to say what it leaves behind.

What is built and green: `cargo test --workspace` on `5ac2c91` (15 tests, one binary), and
`cargo fmt --check`.

## The measurement that says where the cut lives

`.dev/tool-cut-check.sh` drove the cut through the **tool commands** -- `cli exec --session
probe --command '["sleep","60"]'`, then `send_message`. The server answered after the whole
program:

    11:53:59.888  command ran command=exec session="probe" took=60.040454s

Two reasons, both outside the new code:

- a tool command builds its own context in `crates/server/src/tool_cmd.rs` with
  `cancel: CancellationToken::new()`, a token nothing ever cancels, so
  `interrupt_session` cannot cut one either;
- an idle session's `send_message` takes the turn-start path in `commands.rs::deliver`, and
  `note_message()` -- the only thing that bumps the inbox epoch -- is called on the *inject*
  path alone. A message that starts a turn is not an arrival any wait can see.

So the surface the cut actually covers is a tool call **inside a turn**: `agent.rs` builds
the context with the turn's token and the session's `inbox_signal()`. Nothing has observed a
cut there yet; the check was aimed at the wrong surface and can only ever be red.

## Open question, for the user

Should a tool command be cuttable too? If yes, the two places above change, and "a message
wakes exactly one wait" has to be re-read: while a tool command waits, a message would both
end that wait and start a turn. If no, the cut is a turn's rule and the verification needs a
turn -- the `fake/flash-fake` provider the other `scripts/*_selftest.sh` use.
