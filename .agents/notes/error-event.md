# WIP: an error is a session row

A failure is one stored row of kind `error` -- drawn red in the session window and sent
to the model -- instead of a reader-only `notice` plus an injection bolted onto one
failure site.

Where: `store.rs` (`Kind::Error`, `wire_item`), `transcript.rs` (pictures match),
`commands.rs` (`row_event` -> a session-scoped `Event::Log`), `agent.rs` (`Agent::error`
and the turn panic write and draw it), `docs/notes/server-agent.md`,
`docs/notes/server-store.md`.

Verified: `.dev/verify/567fe1e8-error-row.json` (real server + `scripts/fake_llm.py`
refusing one model, a private home, 17/17 checks).
