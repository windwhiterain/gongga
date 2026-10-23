
---

# The claims that did not match the code

Each one was in a comment that had been moved to `docs/notes`. Fixed where the
code was wrong, reworded where the note was.

- **`protocol` — `FIN` was sent and never read.** A closing endpoint emitted a
  `FIN`; `on_frame` only looked at `SYN`, `ACK` and `RST`, so the peer kept
  retransmitting into a connection that was gone until `idle_timeout` (60s).
  `on_frame` now ends the channel with `Closed` on `FIN`, and
  `a_fin_ends_the_reading_side` pins it. This is the one behaviour change here:
  a close now reaches the other side immediately instead of after a minute.
- **`protocol` — `Flags::PING`** was neither sent nor read; keepalive is a bare
  `ACK`. The constant is gone, and so is the flag in the header tables here and
  in `DESIGN.md`.
- **`server` — `Tuning::compact_threshold`**: `None` is the default percentage of
  the window, and the percentage falls back to `compress_above_tokens` only when
  no window is known. The doc said "the server-wide token budget".
- **`server` — the command count.** The note claimed five commands and listed
  them; the catalog has six. The sentence is gone: a count that the catalog
  already answers is not a claim about anything.
- **`cli` — `Connection`** was described as something a front-end flattens to get
  the flags without the commands. Nothing does: `tui` reuses `cli::Args`. The
  note now says what is true — both binaries flatten it into `Args`, and the
  separation exists so the resolution rules have one definition.
- **`bridge` — `clip`** cited `DESIGN §5`; the byte-truncation story is §4. The
  unlabelled `【待确认】` at the end of `Dedupe` is gone: what it was waiting for
  nobody can now say, and the paragraph above it makes the whole argument.
- **`tui` — `to_document_end`** named two history callers that do not call it;
  `restore_input` is the only one.
- **`tui/examples/frame_cost.rs`** was said to be an unfair comparison. It is
  not: the baseline reproduces the original `to_string().len()`, which is a byte
  count, and the corpus is ASCII. The note now says so, including that the old
  renderer over-measured wide characters — the width bug is `Log::window`'s, not
  the benchmark's.

Boilerplate preambles left over from the split ("Reasoning behind the comments in
`<path>`, in code order." — the same sentence in every note) were deleted;
`docs/notes/README.md` says what the directory is, once.
