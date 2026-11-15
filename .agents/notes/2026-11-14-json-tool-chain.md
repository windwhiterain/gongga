# JSON tool chain: the verdicts, the contract, the two slices

2026-11-14, session `escape`. WIP; nothing here is shipped yet.

## What the user decided

- `write`/`edit` become **server cmds**, not tools with their own Rust.
- **No line-number edits.** The anchor is what verifies that the file is the one the
  model thinks it is; a line number always "matches".
- The saving is bought instead by **eliding the middle**: a hunk's `old` is a *list of
  segments* that must occur in order; the span from the first segment's start to the
  last one's end is replaced. One segment is today's exact anchor, so there is one
  shape and no special case. Precedent: Cline's `// ... existing code ...`, Morph's
  `code_edit` (a second model applies it -- not copied: deterministic matching only).
- **JSON from end to end; no command line except `exec`.** A word that carries payload
  is what made heredoc hell possible.
- The harness's Python surface **ships with the content** (`ship/python/` -> `<home>/lib`),
  not `include_str!`: editing Python must not need a rebuild.
- No bootstrap copy of it in the binary: one source of truth, or none.

## The contract both halves implement

1. `ship/ship.toml`: `python = "python"` -> `<home>/lib` (tree, same reconcile/base as `skills`).
2. `protocol` 11 -> 12.
3. Content holds `gongga_skill.py`, `gongga.py`, `gongga_cli.py`; the main repo's `python/` goes.
4. `python <home>/lib/gongga_skill.py`, no argv payload; stdin is one line:
   `{"skill": "<dir>", "function": "<name>", "args": {...}}`.
5. `run_skill`'s `cmd.args` are the function's kwargs -- no flattening to words.
6. Deleted: `call_words`, `bind`/`CONVERT`/`NAMED`/`_as_bool`/`_as_json`, `pylib::install`,
   `PACKAGE_RUNNER`.
7. Nothing but `exec` carries payload on a command line.

## Slices (one writer each)

| slice | session | worktree | files |
| --- | --- | --- | --- |
| content | `escape/content` | `.worktrees/ship-json` | `ship/` only |
| server | `escape/server` | `.worktrees/json-tool-chain` | main repo only |

## Merge order, and why

`scripts/hooks/pre-commit` runs `scripts/ship.sh --check --local`, which compares the
build's protocol with the content's. The bump cannot pass while the pushed content still
says 11, so:

1. content prepared (committed locally, **not pushed**);
2. server green, protocol line edited but **not committed**;
3. push content (`scripts/ship.sh -m`), then commit the bump, then merge both.

Pushing content first is the documented cut-over: an 11 build walks first-parent history
back to the newest commit that speaks 11 and keeps installing it.

## Still open

- `ship/skills/file/` (the `write`/`edit` themselves) + rows + the paging question
  (`read` earns its place only by saying how much was *not* shown).
- Segment-list `edit`: inclusive span, per-segment uniqueness, refuse on ambiguity, and
  the answer must report how many lines the elided span covered.

## Landed 2026-11-14

- content `ee9e844` pushed to gongga-ship main: `ship/python/` (three `.py`), `python = "python"`,
  `protocol = 12`, and the contract asks in JSON (argv refused, exit 2).
- main repo `07426b9c`: server reads the contract from `<home>/lib`, feeds the request on
  stdin, `pylib.rs`/`include_str!`/`PACKAGE_RUNNER`/`call_words` gone, `python/` deleted,
  `crates/server/ship.toml` at 12 (committed by the hook that checks drift).
- evidence: `.dev/verify/baa1dea2-skill-json.json` -- a dict arrives as a dict, an edit to
  `<home>/lib/gongga_skill.py` takes effect with no rebuild, and the offered channel tools
  work again.
- still open: `write`/`edit` /`read` as the cmds themselves (segment-list hunks); and SPEC
  still says Python cmds come from "a shipped uv python package", which is no longer what
  ships (plain `.py`, no `pyproject.toml`) -- the owner decides which side moves.

## Landed 2026-11-14 (second slice)

- content `aa70fef`: `ship/python/{pyproject.toml,uv.lock}` restored (a real uv package),
  and every content skill is a package -- `handoff` (read, handoff), `image` (show),
  `remote` (install/configure/check/verify/list_remotes/remove), `self-control` (ask),
  `subagent` (self_config, new_subagent); `providers`/`prefix-cache` stay prose.
- main `f14b6569`: the server runs a skill only as a Python call; a body and a declared
  entry are refused, each naming what to change. The entry machinery stays for the
  client (`gongga-cli skill`), which is not the tool-call chain.
- proof: content packages import and publish (5/5), `scripts/skill_call_selftest.sh`,
  `scripts/ship.sh --check --local` 0 (in agreement).
- known loss: a machine/community skill with an entry (`general/web-search` .sh) is no
  longer reachable through `run_skill` -- only `gongga-cli skill` runs it.
- NOT yet live: the running server is the old build; `evolve` is what installs this, and
  it cancels the turn that asks, so the owner schedules it.

## `write`/`edit`/`read` landed 2026-11-14

- content `9d797df`: `ship/skills/file/` -- `read(path, offset, limit)` says `total_lines`/`whole`,
  `write(path, content)` is atomic and keeps a BOM, `edit(path, edits, all)` replaces the span
  between N ordered segments (both ends included), each must be unique, ladder exact -> leading
  whitespace -> refusal naming the closest line, all hunks or none, and the answer reports
  `removed_lines`/`first_removed`/`last_removed` so a span that ate 300 lines is visible.
- the contract now reads a skill-raised `ValueError` as a refusal (one line, exit 2) and only
  anything else as a crash; that is the difference between a bad call and a broken skill.
- evidence: `.dev/verify/2026-11-14-file-skill-selftest.py` -> 6/6 ok (hostile bytes, paging,
  elided middle, miss leaves the file alone, BOM+CRLF, batch all-or-nothing).
- measured: **one `exec` args word is cut at 8186 bytes**; a 9.5KB source had to be written in
  two calls. That is the concrete case `write` exists for.
- still open: the live home runs a new build with an old `<home>/lib` contract, so package
  skills (including `new_subagent`) are unusable there until a restart/evolve installs it.
