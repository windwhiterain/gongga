# Ship is the only source of prose (WIP)

The last step of moving the shipped content out: nothing in `crates/server/` holds
prompt text any more, and nothing in the repository holds skills.

User's decisions this came out of (previous rounds): skills ship from a separate
repository, pinned by commit, merged with git against the reader's own edits; the
key lives in `server.key`; a fresh machine with no ship source refuses to start.
`index_skill` is the discovery tool. What was left was S5: the embedded copy.

## What was removed

- `crates/server/default.config.toml` -- the compiled-in starter, and with it
  `prompts::SHIPPED`, `prompts::starter`, `prompts::Value`, and
  `Config::write_default_if_absent`. `toml_path` went with the writer that was its
  only caller.
- `.agents/skills/` -- all nine, each already in the ship repository. `latency` was
  untracked there and was the only file lost by the delete, and it is the same
  content in `skills/general/latency` (verified by a path-normalised diff of every
  skill before deleting).
- `AGENTS.md` no longer lists the skills or tells anyone to grep for them; it points
  at `index_skill` and says the list is not written here on purpose.

## What replaced the fallback

`ship::config_text()` reads the pinned `config/config.toml` out of the home's own
`ship.git` cache (`cat-file blob`), and `prompts::install` layers the home's
`[prompts]` over it. The prose therefore has one source that moves when the pin
moves, and no second copy that can disagree with it.

That fallback is what keeps an existing home alive. The reader's own `config.toml`
predates ship, so ship classifies it `Keep` and never writes over it -- and it has no
`[prompts]` section. Before this change such a home would have had *no* prose at all
to fall back on.

`Prompts::REQUIRED` + `main.rs`'s bail are the check that replaces the embedded copy:
a name neither the home nor the pinned content supplies stops startup instead of
sending an empty system prompt.

`main.rs` now reads the config **twice**: once for the home (the lock and the install
need it before any file exists), and once after the install, because the server should
run on the file the home was handed rather than on the loader's guess at it. `--db` is
re-applied in both, so it lives in `config_from` rather than inline.

`Config::config_path` was added while doing that: `prompts::install` used to be handed
`<home>/config.toml` regardless, so `--config /tmp/other.toml` would have taken its
values from one file and its prose from another. The templates now read the file the
loader read.

## What the tests had to give up

Six tests asserted *sentences* out of the compiled-in file
(`config.rs::the_instructions_are_well_formed`,
`the_instructions_send_the_agent_through_the_tool`, `a_size_refusal_says_how_to_fold`,
`a_text_only_model_reports_why_it_cannot_look`, the two context-notice tests, and
`store.rs`'s interrupted-call wording). Prose in another repository cannot be pinned
from here without turning the test into a build dependency, so the tests moved to the
mechanism and the prose-follows-the-pin claim moved to a live run.

To do that, the functions that render prose now take `&Prompts`: `cap_output`,
`image_refusal`, `vision`, `with_size_hint`, `constitution`. Production callers pass
`prompts::shared()`. `store.rs::interrupted_call` keeps reading the global -- it is
buried in history-building, and threading prompts through `Store::history` would mean
a store that cannot be opened without a config. Its test now asserts that the closing
row *exists and is ordered*, which is store's own claim.

## Verified

- Gates: `cargo test --workspace` 407 passed / 0 failed, `test-layout-check` 0
  violations, `comments-check` "no comment compliance violations found".
- Live, fresh home (`.dev/s5fresh`, fake LLM on 8791, isolated roster):
  `shipped content pin=350e52476b written=10 fetched=true`, `overridden=[]`,
  `.ship-pin` for both roots, and one turn whose request carried `instructions` of
  9306 characters starting `You are gongga, a coding age…`, including the injected
  `## 技能` from this repository's `AGENTS.md`.
- Live, stale home (`.dev/s5stale/config.toml` written by hand, no `[prompts]`,
  `ship.git` the only prose on the machine): install reports `written=9` with the
  home's config `kept` and no `.ship-pin` for the home root, the server still starts,
  and its request carried the same 9306 characters. That is the fallback working, on
  the exact shape of home the reader already has.
- Smoke servers and the fake LLM were killed; `tasklist` shows no `gongga-server.exe`
  and nothing listening on 18950/18951/8791.
- `--config` elsewhere: booting `--home .dev/s5boot --config .dev/s5other/config.toml`
  where that file holds only a `[prompts]` override logs
  `file=...s5other\config.toml overridden=["size_hint"]`, while the same home without
  `--config` logs `overridden=[]`. Prose and values now come from one file.

## Not done

- SPEC has nothing yet about templates, ship, skills or injection (user: 先不写).
- The prose-asserting tests are gone rather than rewritten against the ship repo; if
  that guard is wanted, it belongs in the ship repository's own checks.
- Multi-instance addressing (`--bind :0` and a `<home>/endpoint` file the CLI can
  read) is still undecided; today a shell reaches *its* server through
  `$GONGGA_SERVER` and anything else through `--server/--home/--key`.
