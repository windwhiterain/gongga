# `crates/server/src/tools/index_skill.rs`

## module doc

One table: every skill this server can offer, with the line that says what it is for
and the path that runs it. It exists because the alternative -- telling the model in
the prompt to `grep` the skill directories -- makes each session re-implement the same
discovery, and gets it wrong in whichever way the repository layout moved since the
prompt was written.

## PROJECT, HARNESS, GENERAL

The bucket is reported in the reader's own words rather than as a boolean, because the
useful question after "does this task have a skill" is "whose skill is it": a harness
skill describes this server and is reinstalled from the pinned content, while a general
skill belongs to the machine and may have been edited by whoever sits at it.

The project bucket is the checkout the session is working in, `<cwd>/.agents/skills`.
It is the third and it comes first, because it is the most specific thing that can
hold a procedure: a repository that says "here, do it this way" outranks the home it
sits in. Reading the cwd is the whole reason `installed` takes one: the same server
serves sessions in different checkouts, and a skill in one of them must not appear in
another's table.

`installed_skills` in the command layer walks the same three roots in the same order,
from the client's own directory, so the names a reader completes are the names the
server will find.

## description

The description is the whole argument for calling it: *before* deciding a task has no
skill, and read a body only when its line matches. Lazy discovery only pays if the
cheap step is the one that is mandatory, and a model that has to be convinced to look
at a table will not look at it.

Takes no arguments, so `additionalProperties: false` with an empty `properties` is the
correct schema rather than a placeholder for a filter that would invite a wrong guess
about which bucket matters.

## frontmatter

`---`, then `name:` and `description:` lines, then `---`: the same rule every skill
file already followed for the other harnesses, and read here as two lines rather than
as a YAML parser. A body without frontmatter still appears in the table with an empty
description and its path, because a skill nobody described is a real file the reader
may still want -- and the missing line is the bug worth seeing.

A folded or quoted description on one line is read as written; multi-line YAML values
are not attempted. That is a documented shape of the input, not a limitation of the
reader: a description that needs several lines is a body.

## both roots, and the path each row carries

Rows point at `$GONGGA_HOME/skills/<name>/SKILL.md` or `~/.agents/skills/<name>` --
the paths the skill's own prose tells the reader to run. Emitting a path relative to a
repository root would be a lie here: the session's cwd is whatever the user was working
in, and the skills no longer live in it.

## empty roots

A root with no skills says so in the output instead of returning a short table. "No
skills installed" and "one skill matches your task" are different facts, and the second
one arriving alone would read as though the install had succeeded.
