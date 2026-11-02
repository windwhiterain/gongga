# a changed entry is patched, not dropped

WIP for `feature/cache-tolerance`, in `.worktrees/cache`. What the prefix cache does
when a file it indexes is edited, and what is still owed.

## the ask

A memory target file changes; measure the change with a diff, and if it is under a
tolerance, do not drop the memory node -- keep it and append the change at the end.

## what the code does now

- `crates/server/src/diff.rs` (new): a unified patch or `None` when the change is over
  tolerance. Verified by applying its own output back before returning it.
- `prefix_cache::Item` keeps `text`, so the next plan diffs against the bytes the
  provider was actually sent, not against the file's last version.
- `prefix_cache::plan` takes `Wanted { key, text, tokens }` and a `tolerance`, and
  answers with `Plan { entries, revisions }`: each entry with the text to send under it,
  and a patch per entry that was kept. `forget_unwanted` therefore keeps a tolerated
  item and its chains.
- `agent::index_of` sends the kept text and appends `reference_revision` after the
  index, one block per entry under its spelling. `Agent::index_tolerance` returns `None`
  -- no patching, old behaviour -- when the template is missing or the knob is `0`.
- `tools::index_skill` passes `None`: rows are json.
- `[agent] index_change_tolerance`, `GONGGA_INDEX_CHANGE_TOLERANCE`, default `0.2`.

Verified by hand with throwaway probes (deleted): one-line edit, two distant edits, two
insertions, two deletions, identical, full rewrite at tolerance `1.0`, CRLF flip, and a
layout read back from the database and planned from again. The probes are not in the
tree: SPEC/AGENTS forbid writing tests without the user's permission.

## owed

1. **No measurement against a real provider.** The 98.6% / 5120-5504 numbers quoted in
   `docs/notes/server-prefix-cache.md` are from the earlier probe run recorded there;
   what a patch appended at the end of `instructions` is worth has not been measured
   again with this code. The claim being made is narrower and is checked: the entry's
   text is unchanged, so the characters before the appended block are the ones that
   were sent last time.

## how the template reached the pin

It did not go the way it was supposed to. The machine had no route to the content
repository (`flm` absent, `github.com` resetting the connection), so
`reference_revision` was written into `ship/config/config.toml` of the main checkout and
left uncommitted, with the note saying the tolerance was dormant until a release.

The way out was the `proxy` skill: `proxy serve`, `proxy check github.com` →
`ROUTED VIA relay` (a decision the proxy logged itself, not an environment variable
believed), and `git ls-remote` through it in 1.5s. By then another session had committed
and pushed the whole working tree as `b33ca4b` -- the template with it -- and bumped
`crates/server/ship.toml` to that commit. So the release happened, but not under a
message that says what it published.

That is worth knowing for the next one: a file left uncommitted in the content
repository is a file any other session's `git add -A` will publish under its own
message.

## what was checked, and how

`cargo build --workspace` and `cargo test --workspace` clean (the workspace has no
tests, so this is a link check); `test-layout-check --config test-layout.toml` 0
violations; `comments-check --config comments-check.toml` at the same violation set as
`HEAD` -- the repository fails that check on `main` already, and the five sections this
change made too long were shortened to fit rather than left as new ones.
