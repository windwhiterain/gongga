# WIP: the completion a value's own type decides (feature/json-argtypes)

## What the user asked (direct)

"client cmd 补全还缺少 str 自动补引号, bool 补全 true/false。反正你把补全的体验
全面检查一下" -- and a full check of the completion experience.

## What was measured before editing (real TUI, `scripts/tui_live.py`)

- `/send_message{` + Tab -> popup offers `"message":}`; a second Tab at the value
  place offered **nothing**: a string value had to be quoted by hand.
- `/cancel{` + Tab -> `"keep_alive":}`; then Tab at the value place offered
  **nothing**: a root bool argument had no `true`/`false` (the nested
  `SessionConfig` leaves had, because they publish a `Static` list).
- `/set_session{` Tab Tab -> `{"id":"main"}`: a session name *was* quoted, so the
  quoting that existed came from the candidate's text, not from the type -- and
  `ctx_limit`'s `"0"` went in as the number `0`.
- A typed burst (`/cancel{` then Tab at once) produced `/cancel{}{`: the answer to
  the previous question was applied to text typed after it.
- `/set_session{` listed `"live_compact_enabled":""}`: the server published a
  boolean argument as a string, because `commands::type_of` did not name it.

## What changed

- `StructField.type_of` (SPEC's spelling, `protocol::argtype` reads it) -- published
  for every `SessionConfig` leaf and for every root argument.
- `client/src/leaf.rs`: `Leaf::{Text, Switch, Other}` and the two writers (a JSON
  token for a partly typed object, a value for a sticky fill). One reading, three
  users: `keys` opens a string leaf as `"name":""` with the caret between the
  quotes, `values` writes the published candidate through it, `owed` offers what a
  leaf with no list still owes (a closed fragment, or `true`/`false`).
- `json_value::Spot.quoted`: a token between two quotes owes nothing more; one
  whose closing quote was deleted is finished with that quote.
- `Tui::set_completion` drops an answer whose `line`/`cursor` are not the window's
  own: the `Completion` frame now carries the line and caret it answers.
- `commands::type_of`: `live_compact_enabled`, `live_compact_interval_tokens`,
  `replay` were declared strings; `get_messages` now reads `replay` through
  `flag_of`, the one boolean reader.

## Findings, not fixed (reported to the user)

- The popup draws the closers a candidate appends (`"message":""}`), which is what
  is inserted but reads like part of the name.
- A list-valued leaf (`list[str]`) offers nothing, and the comma after a value is
  typed by hand.
- The client's own commands (`set_render`, `set_render_session`, `connect_server`,
  `serve`) publish every argument as `str`, because their readers take words
  (`Args::word`); publishing `bool`/`int` there would need those readers to read
  values, so `--show` completes to `"true"` rather than to `true`.
- `scripts/tui_live.py` wipes its home with `rmtree(ignore_errors=True)`: a
  previous run's lingering process leaves a half-wiped `ship`, and the next run then
  dies on "ship is not a clone". Worked around with `--keep-home`.

## Verification (this sha)

- `cargo build` (default members) 0; `cargo fmt --check` 0
- `comments-check --config comments-check.toml` 0; `test-layout-check` 0
- `bash scripts/skill_contract.sh` 0; `bash scripts/cmd_selftest.sh` PASS
- `python scripts/tui_live.py --scenario completion-through-client`: every expect
  reached (string field opens `""`, a session name completes quoted inside it, a
  bool argument writes `true` unquoted)
- `bash scripts/ship.sh --check --local` exit 1: content main speaks protocol 13,
  this build reads 12 -- the content moved after this build; nothing here touches
  ship, and a server walks back to the newest commit it can read.
