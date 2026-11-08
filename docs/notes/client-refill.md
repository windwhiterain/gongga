# client/window_refill.rs

What a window asks for after a greeting, a scroll, or a completion that
answered a peer's directory. Lifted out of `crates/tui/src/run.rs` on
2026-11-08: `declare`, `answer_completion`, `answer_transcripts` and
`answer_older`. None of them needs a terminal.

## WindowFacts

The four refills used to take `&Tui` or `&mut Tui`. They now take
`&dyn WindowFacts` (and `&mut` only for `answer_completion`, which refreshes
the completion). `Tui` implements the trait with five one-line forwarders to
methods it already had, so what the operator sees is unchanged.

This is **not** a cross-process query protocol. Verdict 4 puts the window
journal in the client: once the index moves here, the client implements
`WindowFacts` itself and the refills read local state. `CursorMessage` stays
client-internal.

## Still owed

`sync` and `settle` still take `&mut Tui` -- they set the block size and
sticky session, take settled answers and restore the input line. The
`interactive` loop itself, and the `log.rs` / `panes.rs` index behind
`WindowFacts`, are the next steps. See `DESIGN.md` §1.
