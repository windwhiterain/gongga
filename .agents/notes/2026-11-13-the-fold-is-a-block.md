# tui: a fold is a block where SPEC drops `(line)` (2026-11-13)

## Goal
User: "the TUI renders some events that are not limited to a single line as one
line; fix it strictly per SPEC". SPEC is the source of truth.

## The reading
SPEC's `events` block annotates each summary item:

| SPEC | shape |
| --- | --- |
| `reasoning{beginning (line), ending (line)} (vertical)` | two lines |
| `harness_injection{beginning (line)}` | one line |
| `toolcall{tool_name·[*summary_args]·description}` | no `(line)` |
| `tool_result{ok, time}` | no `(line)` |

`(line)` came off `toolcall` and `tool_result` in `0d9d6df2` (Nov 9, "spec"): until
then both read `(line)`, and the TUI kept cutting them to one row with `…`. So the
shapes SPEC gives no `(line)` are blocks, and this is the same reading `23305ad4`
already applied to `[cmd/cmd_result]` when its `(line)` was dropped.

## The bug
`Entry::folded_row` composed ONE row per folded item (`cut(...)` for every kind). A
call's summary -- `name·description`, plus the args marked `summary` -- was therefore
cut at the window edge, and the rest of the description was gone from the log.

## The fix (crates/tui/src/log.rs)
- `Entry::fold: Vec<String>`: the summary rows, made once at `reflow` instead of
  derived per frame. `reflow` = `layout` + `refold`; `seal_quiet` and a sealed
  `append` refold, so the rows follow the body.
- `summary_rows`: `(line)` kinds are cut as before (reasoning keeps its beginning and
  ending, one row when one line is both; an injection its beginning); a kind with no
  `(line)` (`toolcall`, `tool_result`) is wrapped through `hanging_indent`, so
  `block_size` caps it and an opened box still scrolls.
- `summary_text` reads `Head::summary` rather than `tag`: `layout` cuts the label to
  the one row it rides on, and wrapping an already-cut label only moves the `…`.

## Follow-up: the row's own order and spelling
User: SPEC says the summary args come before the description, joined by `·`, with no
arg name -- fix that too. SPEC's `toolcall{tool_name·[*summary_args]·description}` is
read literally, so `Head::summary` is now `name · value · value · description`
(`crates/protocol/src/message.rs`): values bare, because the name an argument is read
under is the open item's row, and an empty part is dropped so no mark is stranded.
`Head::title` stays the open item's first row (`name·description`), which is what
`detail()` opens with. `frontends/avalonia/Wire/WireTypes.cs` mirrors the rule in C#
and was changed with it; `docs/notes/protocol-message.md` and `client-lib.md` quote the
new spelling.

Proved in a frame: `▸ exec·bash·check the state` for
`{"bin":"bash","args":["-lc","seq 1 3"],"description":"check the state"}` -- the
arg name is gone, the marked value sits between the name and the description.

## Verification
- `scripts/tui_live.py --home .dev/live-home7`, one scripted turn with a described
  call: frame shows the call over two rows
  (`▸ bash·check the state of the thing carefully so that this description is long enough to n`
  / `  eed a second row`), reasoning still one row for one paragraph, user message and
  `cmd_result` unchanged (multi-row).
- `scripts/tui_live.py --scenario wheel-after-unfold` and `--scenario single-stream`:
  both scripts ran to the end (fold, click-to-open, box wheel, block_size = 2).
- `cargo build`, `cargo fmt --check`, `comments-check` (log.rs back at its 2
  pre-existing violations, none added), `test-layout-check`, `skill_contract.sh`,
  `ship.sh --check` (in agreement).

## Open
- `ship/config/config.toml` in the content checkout was already modified when this
  session started (prompt prose being moved out of the binary). Not mine, left alone.
- The rest of the `abstract` vocabulary in code comments predates SPEC's rename to
  `summary`; only the fold's own comments were renamed here.
- `frontends/avalonia/ViewModels/ViewBase.cs` still shows a block's summary as one
  `Abstract` string, so the same reading applies there. This session only fixed the
  TUI, which is what the user asked about.
