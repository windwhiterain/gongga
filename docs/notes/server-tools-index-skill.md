# `crates/server/src/tools/index_skill.rs`

## module doc

One table: every skill this server can offer, with the line that says what it is for
and the path that runs it. It exists because the alternative -- telling the model in
the prompt to `grep` the skill directories -- makes each session re-implement the same
discovery, and gets it wrong in whichever way the repository layout moved since the
prompt was written.

## the walk

The bucket words and the walk itself moved out of this file and into `crates/skill`, which
is now the one reading of "where a skill is" for the table, for `@skill:`, for the command
layer's completion, and for `gongga-cli skill`. The reason is in that note: three callers
with a comment saying they walk the same roots in the same order is three chances for the
comment to become false.

What stays here is what is the *tool's* business: the table, its order, and the sentence a
remote session is handed instead of a project layer.

The project bucket is the checkout the session is working in, `<cwd>/.agents/skills`, and
it comes first because it is the most specific thing that can hold a procedure: a
repository that says "here, do it this way" outranks the home it sits in. Reading the cwd
is the whole reason `installed_for` takes one: the same server serves sessions in different
checkouts, and a skill in one of them must not appear in another's table.

## description

The description is the whole argument for calling it: *before* deciding a task has no
skill, and read a body only when its line matches. Lazy discovery only pays if the
cheap step is the one that is mandatory, and a model that has to be convinced to look
at a table will not look at it.

Takes no arguments, so `additionalProperties: false` with an empty `properties` is the
correct schema rather than a placeholder for a filter that would invite a wrong guess
about which bucket matters.

## ordered

The table is a permutation of `installed`, and the order is `prefix_cache`'s: a tool
result sits in the transcript, the transcript is the request's prefix, so a table
whose bytes match the last one is bytes the provider may still have cached, and a
table re-sorted for a fresh reason is a divergence that costs every token after it.

The layout is the home's, in the database, one row (`prefix_cache::plan`): a provider
caches a prefix per endpoint rather than per conversation, so the second session to
list the same skills is the one that collects -- and so is the next run of the server,
which is why it is not a field of anything that dies. It is shared with every other
caller that has an order to choose, which is safe because a chain carrying a row
outside the current set is not a candidate.

The key is the row's `SKILL.md` path, not its name: a name can be in more than one
root, and two directories can even declare the same `name:` in their frontmatter, so
a table keyed by name would silently drop a skill. The path is unique by
construction. The size is the row's serialized length, the text the request carries.

`frontmatter` and `installed` are untouched by this: precedence is still the walk's
order (project, then home, then machine), so `@skill: <name>` still opens the same
file. Only the table's order is the layout's.

A reworded description is a row whose text changed, and the prompt index would keep such
an entry and append a patch for it -- `git diff`-shaped prose, appended after the index.
This caller passes no tolerance instead: a row here is one line of json, where a patch
saves a handful of characters and costs a reader a parse. See
`docs/notes/server-prefix-cache.md`.

## both roots, and the path each row carries

Rows point at the `SKILL.md` the table was read from -- `<home>/skills/<name>/SKILL.md`,
`<cwd>/.agents/skills/<name>/SKILL.md`, `~/.agents/skills/<name>/SKILL.md`. Emitting a path
relative to a repository root would be a lie here: the session's cwd is whatever the user
was working in, and the skills no longer live in it.

It is also the anchor a body's `SKILL_DIR` stands for, which is why the row carries the
whole path rather than the directory: a reader that has the table has the one absolute
fact a portable invocation needs.

## empty roots

A root with no skills says so in the output instead of returning a short table. "No
skills installed" and "one skill matches your task" are different facts, and the second
one arriving alone would read as though the install had succeeded.
