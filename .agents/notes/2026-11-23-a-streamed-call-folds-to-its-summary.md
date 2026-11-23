# A streamed call folds to its summary

The row an `edit` call shows while it streams is not the row it shows once the call
is known, and the second one carries no arguments at all.

Measured on build `8d0f6975c34c`, a `100x30` window, `scripts/tui_live.py` driving the
real TUI against a stand-in endpoint that streams one `edit` call. While the
arguments arrived:

```
▸ edit({"path":"crates/server/src/a-rather-long-file-name.rs","description":"a description
  ong enough that the folded row cannot fit it beside the arguments and must wrap onto more rows than one"})
```

Two rows, 88 and 63 columns, contiguous with the arguments as written: the wrap is
the window's and nothing is lost at the break. A continuation row starts at the
window's left edge, without the mark, which is why a first reading took it for two
lost characters.

Once the call completed, the same block read:

```
▸ edit·a description long enough that the folded row cannot fit it beside the arguments an
  d must wrap onto more rows than one
```

`path` and `edits` are gone from the row. That is not a wrap that failed: it is
`Log::describe` (`crates/tui/src/log.rs`) throwing the streamed body away
(`entry.text = String::new()`), attaching the structured head, and sealing the block,
after which a folding kind draws `Head::summary()` alone. `edit`'s `summary_keys` in
`crates/server/src/agent.rs` is empty, so that summary is the tool's name and the
call's description; every argument lives in the box a click opens
(`Entry::section_text`).

So a reader sees the arguments for as long as the call streams -- tens of
milliseconds against a fast endpoint, and nothing at all when the provider sends
whole arguments instead of deltas -- and never afterwards.

## Open choice

Nothing here is changed yet: which row a call should keep is a SPEC shape question, and
`SPEC.md` is not edited without the maintainer's word.

The three shapes, and what each costs:

1. **Leave it.** The folded row stays `tool_name·description`, and every argument is one
   click away in the box. This is what the code does now. It costs the reader a gesture
   whenever the description does not already say what was passed -- `edit`'s `path`
   being the case that started this note.
2. **Mark an argument `summary`.** `summary_keys` in `crates/server/src/agent.rs` already
   decides this per tool, and the row is built from it: naming `path` for `edit` puts it
   in the folded row without touching a front-end or the wire. It costs a folded row
   that no longer fits one line for a long path, which the block already wraps.
3. **Keep the streamed body.** The row would hold the arguments as the model wrote them
   and fold to the summary only when opened. This one changes SPEC's `toolcall` shape
   and `Log::describe`'s invariant that the fields are the body, so it is a decision for
   the maintainer rather than a fix.

The measurement is the argument for bothering at all: the streamed row is visible for
about a hundred milliseconds against a fast endpoint and never for one that sends whole
arguments, so what a reader is left with is shape 1 or shape 2.