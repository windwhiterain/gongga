# Prompt templates, live roster, injected rules (WIP)

User's four requirements, decided in this session:

1. `~/.agents/llm.toml` reloads live.
2. `AGENTS.md` is injected into the prompt automatically.
3. All prompt prose, the system prompt included, is string templates in the gongga config.
4. All of it is live, and the history of a conversation already under way is never modified.

Chosen: hand-rolled `{{name}}` interpolation (no template dependency); re-read before
**every model request**, not per turn; templates cover prose only, so tool JSON
descriptions stay in code; the defaults live in one repo file that a new home is
copied from.

## What was built

- `crates/server/default.config.toml` -- the shipped starter config, now also the
  authority for all eleven templates. Valid TOML as committed: every machine value
  is a quoted `{{placeholder}}`, and `prompts::starter` fills the part above
  `[prompts]` and appends the rest byte-for-byte. A placeholder inside a template
  is a later request's business, not startup's.
- `crates/server/src/prompts.rs` -- the overlay (home `[prompts]` over the shipped
  set), a size+mtime stamp so a steady server re-reads nothing, `substitute`,
  `constitution`, and a process handle installed at startup because `store.rs`,
  `tools/bash.rs` and `config.rs` all need prose and carry no config.
- `LlmConfig` lost `instructions`; `LlmClient` lost it too, and `build_body`/
  `stream` take the instructions of *this request*. The eight literals that used
  to be Rust strings are templates: `config.rs::default_instructions`,
  `agent.rs::context_notice`, `llm.rs`'s summary preamble and size hint,
  `store.rs::INTERRUPTED_CALL`, `config.rs::image_refusal`, `bash.rs`'s two cap
  notices. `Prompts::defaults()` is what the two old instruction tests now pin.
- `providers.rs`: `roster()` returns `Arc<Catalog>` and re-reads on a stamp
  change; each read is numbered. New `providers::Handle` is what `Agent` and
  `Dispatch` hold -- see the bug below. `Agent::client_for` keys cached clients by
  `(spec, generation)`, so a reload retires the client that carries the old
  base URL and key.
- `agent.rs`: `Attempt` gained `cwd`; the instructions are assembled per step from
  the template plus `~/.agents/agents.md` and the session directory's `AGENTS.md`,
  injected into `instructions` and never stored.

## The bug the smoke run caught

First cut put `Arc<Catalog>` in `Dispatch.providers`, assigned once at startup --
which freezes the roster exactly as `&'static` did. `set_session --llm p/m2`
answered `known: m1` after m2 had been written into the file. Fixed by the handle
that forwards to `roster()` per question; the same smoke run then passed.

## Verification

- Gates: 393 workspace tests, `test-layout-check` 0 violations, `comments-check`
  0 violations, `cargo build --workspace` clean (the user's instance had exited, so
  `target/debug/*.exe` were writable again).
- End to end against `scripts/fake_llm.py` with `FAKE_LLM_DUMP`, isolated homes and
  ports, and `GONGGA_LLM_CATALOG` pointed at a scratch roster so the machine's own
  file was never touched:
  - new home's `config.toml` = the shipped file with real values (`store = false`,
    `mtu = 1200` rendered bare, `bind` quoted) and `[prompts]` present;
  - request 1's `instructions` = template + `AGENTS.md` text, wrapper included;
  - editing `[prompts] instructions` in the home made request 2 carry the marker
    while request 1 did not, with no restart and no server recompile;
  - history stayed clean: 4 rows (two user, two answer), zero rows containing the
    injected rules or the edited marker, checked through `get_messages` and in the
    database;
  - roster: `p/m1` ok, m2 appended to the file, `p/m2` ok, `p/nope` still refused
    naming the models it does know.
- `prompts::install` logs what the home overrode, and `overridden()` counts only
  templates whose text differs -- a fresh home reports `[]`, not all eleven.

## Left open

- SPEC has nothing yet about prompt templates, the injected rule files, or the live
  roster; the text proposed to the user is not written into SPEC.md yet, and
  `SPEC.md` still describes no `[prompts]` section.
- Pre-existing warnings untouched: `SessionManager::materialise` (test-only),
  `PendingTask.payload`/`created_at` (read only by tests).
- Scratch under `.dev/` (gitignored): smoke target dir, the three homes, rosters,
  the request dump, and the one-shot edit scripts I used for the multi-line Rust
  replacements.
- Not committed.
