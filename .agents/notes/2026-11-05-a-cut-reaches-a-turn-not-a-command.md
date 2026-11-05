# A cut reaches a turn, not a command

The wait for a tool call used to belong to the `bash` body: a `select` over the command, the
session's cancellation token and a 250 ms inbox tick. `exec` and `python` wait the same way
and had no answer, so a `remote_exec` waiting out a long timeout could not be reached by
`send_message` or by `interrupt_session`. The fix puts the wait in `ToolRegistry::run`
(`waited`) and gives each tool `Tool::cut` to say what it leaves behind.

## Where a cut can arrive, and where it cannot

The first check drove the cut through the **tool commands** -- `cli exec --session probe
--command '["sleep","60"]'`, then `send_message`. The server answered after the whole
program:

    11:53:59.888  command ran command=exec session="probe" took=60.040454s

Two reasons, both outside the new code, and both still true:

- a tool command builds its own context in `crates/server/src/tool_cmd.rs` with
  `cancel: CancellationToken::new()`, a token nothing ever cancels, so
  `interrupt_session` cannot cut one either;
- an idle session's `send_message` takes the turn-start path in `commands.rs::deliver`, and
  `note_message()` -- the only thing that bumps the inbox epoch -- is called on the *inject*
  path alone. A message that starts a turn is not an arrival any wait can see.

So the surface the cut covers is a tool call **inside a turn**: `agent.rs` builds the context
with the turn's token and the session's `inbox_signal()`.

## The measurement, on that surface

`FAKE_LLM_TOOL` makes the repository's own fake endpoint ask for one tool and then answer,
which is a real turn with a real shell and no provider in the loop. Three scenarios, one
server:

| scenario | what the wait was | result |
| --- | --- | --- |
| message ends an `exec` wait | `["sleep","60"]` | answered 0.3 s after the message, "was stopped: a message arrived for this session" |
| message ends a `bash` wait | `sleep 8; touch marker` | answered 0.6 s, "handed to the background", and the marker was written 8 s later |
| cancel ends an `exec` wait | `["sleep","60"]` | answered 0.2 s after `interrupt_session`, "was stopped: the turn was cancelled" |

Each scenario also checked that the turn went on to read the message, which is the point of
cutting rather than waiting: the news reaches the step boundary that follows.

Run on `feature/tool-cut` with `.dev/tool-cut-check.sh` (throwaway, not committed). The
workspace tests are green on the same tree: `cargo test --workspace` 15 tests, one binary.

## Open question, for the user

Should a tool command be cuttable too? If yes, the two places above change, and "a message
wakes exactly one wait" has to be re-read: while a tool command waits, a message would both
end that wait and start a turn. If no, the cut is a turn's rule, which is what is built.

The other open question is where that check should live: `scripts/tool_cut_selftest.sh`
alongside the other L3 scripts, or nowhere, since it is a test and tests are the user's call.
