# The compact skill became heir: hand the past to a mentor, then empty yourself (WIP)

The user's instruction, with the four mechanics they chose:

1. 重要信息进交接文档。
2. 调用脚本把自己的 transcript **拷贝**到一个 mentor 会话，然后**清空**自己的 transcript，
   再注入一条引导 prompt。
3. 遇到问题可以问 mentor —— 允许真发回合（`send_message`，走模型、花额度）。
4. mentor 就是**一条普通会话**，名字由调用者决定（不自动命名、不是文件、不是新机制）。
5. 发布方式：`gongga-ship` 挪进主仓库当 **git submodule**，然后 push + re-pin。

## What that changed about the invariant

"已经发起的对话记录的历史上下文绝对不修改" was the user's own rule from the template round,
and `store.rs` / `docs/notes` said the transcript is **never rewritten**. `heir` deletes rows,
so the sentence had to be read for what it always meant: *the server* never rewrites a
transcript, and a config reload never reaches into a conversation in progress. Deleting is
allowed when the agent asks for it, in a step where it also writes the note, and only into an
archive it can still query. `#[cfg(test)]` on `set_summary` still encodes the server half.

## What was built

- `ship/skills/harness/heir/` (was `compact/`): `transcript.py` gained `heir --session S
  --mentor M --summary TEXT [--guide TEXT]`. One transaction: create-or-append the mentor
  (inheriting S's model and cwd), write a marker row, copy every row verbatim, **re-read the
  copy and compare `(kind, content)` row by row**, then delete, then set
  `summary/summarised_upto=0/context_tokens=0`, then insert the guide. Any mismatch rolls
  back -- the originals survive and stderr names the seq.
- The guide it injects names the mentor and both ways to reach it, and says **read before
  you ask**: `get_messages` costs nothing, a `send_message` turn costs quota and grows the
  mentor's own context.
- `SKILL.md` now teaches two paths with a table: `collapse` (rows stay, `read --after 0`
  gets them back) vs `heir` (rows gone, mentor holds the only complete copy).
- ship `config/config.toml`: `[agent]` comment, the `[prompts]` header comment,
  `context_notice` and `size_hint` all point at `heir`, and `context_notice` offers both
  commands. `evolve`/`self-control` prose and the ship `README.md` follow the rename.
- Entry points are now `$GONGGA_HOME/skills/heir/transcript` (the wrapper that probes for a
  Python with `sqlite3`), not `transcript.py` -- and the ship repository recorded **every**
  skill script as `100644`, so nothing installed from GitHub was executable. `git
  update-index --chmod=+x` on the nine entry points; that was a latent bug for any fresh
  machine, not for this one (this machine's copies came out 755 because `core.fileMode=false`
  hides the real mode and the local install inherited the working tree's).
- This repo: `store.rs` schema comment, `agent.rs` two comments, `docs/notes/server-agent.md`,
  `server-compact.md` (retitled, plus the heir/rollback/refusals sections and the paragraph
  about what "never rewritten" meant), `AGENTS.md` (the `heir` bullet + the submodule
  re-pinning rule), and the ship test fixture renamed to `heir` so "shaped like the real one"
  stays true.

## Submodule

`git submodule add https://github.com/windwhiterain/gongga-ship.git ship` -- so editing the
shipped content and re-pinning are one change: `ship/` commit + `git add ship` + the `commit`
in `crates/server/ship.toml`. `test-layout-check` still sees 5 packages (a nested repo with no
Cargo.toml is invisible to it). The sibling `C:/resource/gongga-ship` was left in place; the
submodule clone is the one that is now authoritative for edits here.

## Verified

- `python .dev/heir_verify.py` against a database the server itself created (scratch home,
  port 18955): 24 checks, all ok -- mentor created with S's model/cwd, marker + every row,
  byte-identical copies in original order, learner left with exactly one row at seq 1 that
  names the mentor and both commands, summary = note, `summarised_upto = 0`,
  `context_tokens = 0`, an existing mentor row untouched by a second handover, `read` still
  sane afterwards, the four refusals, and **the tamper test**: an `AFTER INSERT` trigger that
  rewrites the copy made `heir` exit non-zero with the learner's rows intact, the mentor
  unchanged, and the old summary still in place.
- Gates: `cargo test --workspace` clean (407+), `test-layout-check` 0 violations, `comments-check`
  no violations.

## Not done / open

- SPEC says nothing about compaction or skills (the user deferred writing it).
- `heir` does not carry the provider-side image `file_id`s forward: a picture sent after an
  heir is re-uploaded. Documented in the skill, not fixed.
- A mentor is a normal session, so nothing stops it from being `del_session`ed and taking the
  archive with it. The skill does not yet warn about that; it probably should.
