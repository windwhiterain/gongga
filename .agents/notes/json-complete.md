# WIP: fully-JSON command completion (feature/json-complete)

## Goal (user, direct)
Replace the Client's command completion with a fully-JSON form, e.g.
`/set_session{"id":"abc","config":{...}}`. The mixed flag/positional + JSON
completion is inconvenient and sticky stops filling. Completion must auto-close
brackets/quotes; the text must never carry a bracket/quote error.

## Architecture (read, confirmed)
- One submit path: `client/src/window_input.rs::submission` (runtime.rs drives it).
- One completion path: `client/src/catalog.rs::Completer::complete`.
- TUI only emits intents (Submit/Complete); it owns no command knowledge.
- Wire `Args = BTreeMap<String,String>`; a structured value travels as its JSON
  text. Canonical JSON->word: String -> itself, other -> compact JSON text
  (server commands.rs::parse_cmd already does exactly this).
- Existing JSON machinery to reuse: `json_value::spot`, `catalog::keys/values`,
  `Candidate::cursor` + `closers` (already auto-close braces).

## Plan
1. Grammar: `/name{json}`. Name up to `{`/whitespace/end. Pure text stays a
   message (@ references unchanged). Legacy `--flag`/positional still PARSES
   (front-end click lines), but completion offers JSON only.
2. commands.rs: add `parse_json_command` (name + JSON object -> Args), strict on
   submit, reusing the canonical JSON->word mapping.
3. window_input: parse JSON form, fill sticky into the JSON Args, echo raw JSON.
4. catalog::complete: drop flag/positional branches; root schema = command's
   ArgSpec list; offer `{` opener then keys/values via spot/keys/values.
5. Balance: completion insertions always closed (closers). Add auto-pair for the
   JSON command context in the TUI editor so typing never leaves an open pair.

## Decisions
- Keep legacy flag parsing as a fallback (no mixing in the popup); JSON is the
  offered form. No forward-compat shims beyond that.
- No new tests (forbidden). Verify: cargo build/fmt + existing checks + read.

## Status (done)
- [x] parser (`json_command.rs`, strict submit / tolerant split)
- [x] sticky (`sticky_additions`, one rule for both spellings)
- [x] completion (JSON-only; names open `{}`, fields/values auto-close)
- [x] balance: every insertion adds exactly the owed closers; quotes paired;
      heals a hand-typed lone `{`. Auto-pair-on-typing not needed: completion
      always leaves balanced text.
- [x] verify: cargo build (default members) exit 0; fmt; comments-check adds 0
      over baseline; test-layout-check 0. No interactive TUI run (no test harness
      permitted); balance invariant established by reading spot/keys/values.
