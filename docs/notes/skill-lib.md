# `crates/skill/src/lib.rs`

## module doc

One reading of a skill directory, shared by the two places that need it: the server's
`ls_skill` table and `@skill(...)` expansion, and the command layer's completion. It exists because the reading had grown two
implementations -- one in `server/src/tools/skill.rs`, one in `cli/src/commands.rs`
-- each documented as "the same roots in the same order as the other one". That comment
is the bug: a third caller appeared (the skill call), and a third copy would have been the
point where the three orders stopped agreeing.

## Bucket

`Bucket::ALL` is the precedence, and it is the only place the order is written. `project`
is the checkout the session works in, `harness` is what the pin installed under the home,
`general` is the machine's, which on this harness is the community clone linked into
`~/.agents/skills`. The words are the ones `ls_skill` already prints and the prompts
already use, so nothing here is a new vocabulary.

The crate is separate from `protocol` on purpose: the wire crate carries the frames, and
a filesystem walk inside it would put the extension point of "which roots exist" in the
middle of the transport. It is separate from `server` because the command layer does not
depend on the server -- `serve` starts a program, and linking the whole harness to
complete a skill name is what that rule prevents.

## Roots

Each field is a directory *of* skill directories, not a skill: `<cwd>/.agents/skills`,
`<home>/skills`, `~/.agents/skills`. `project` and `machine` are `Option` because the
layers can be genuinely absent -- a session whose directory is on a peer has no project
layer on this disk, and a machine whose home cannot be found has no general layer -- and
a placeholder path there would read the process's own directory as a root, which is a
table full of skills nobody installed.

## all, named, find

`all` walks every layer and sorts each one's directories, so the same home draws the same
table twice. `named` keeps every skill a name opens instead of the first one: `find` is
what `@skill(...)` reads, and the rest is what the editor's completion reports as shadowed. A name in two layers is not an error -- the specification says the more
specific layer wins and asks the implementation to say when it did -- but it is also not
something to hide from the reader who typed the name.

`find` is the one resolver, and a name is the whole address. The layer a skill came from is
a field of the row (`bucket`), never a word in front of the name: `skill: file` is the
spelling, and `harness/file` names nobody -- it is refused with the list of names, the
same way a typo is. `Bucket::ALL` is the precedence `find` reads in, which is what makes a
home skill answer before a machine one of the same name. Nothing here resolves a path: a
root is one directory of skills, so how deep a layer keeps its own files is not part of
what a caller says. Each answer is one walk of the three roots, which the catalog affords
because it asks once per fingerprint rather than once per call.

## Skill

A row is the `SKILL.md`, its name, its one-line description and the root it came from;
nothing else about a directory is read here. `dir()` is the anchor the prose's `SKILL_DIR`
stands for: the two must be the same directory, or the body and whatever runs the package
disagree about where its files are.

## frontmatter

`---`, then top-level `key: value` lines, then `---`. Two of the keys are this harness's:
`description` (the table's line) and `name` (what `@skill(...)` resolves). Everything else in
the header is the reader's or another harness's -- including `metadata:`, whose fields are
read by nobody here since a skill stopped declaring which file it runs.

A folded or quoted description on one line is read as written; multi-line YAML values are
not attempted, and that is a shape of the input rather than a limitation worth a parser: a
description that needs several lines is a body. Only top-level keys are read, so an indented
line is skipped rather than mistaken for one of these two.

## shell_path

A path spelled the way a POSIX shell reads it, `C:\a\b` as `/c/a/b`. It started life in
`server/src/tools/bash.rs` as `msys_path`, where it quotes a path into a shell command, and
the server's remote-shell and spill paths call this one. Two copies of an eight-line string
function would drift in the one case that matters -- a drive letter in lower case.
