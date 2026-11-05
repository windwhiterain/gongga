# `crates/skill/src/lib.rs`

## module doc

One reading of a skill directory, shared by the three places that need it: the server's
`skill` table and `@skill:` expansion, the command layer's completion, and
`gongga-cli skill`, which runs the entry. It exists because the reading had grown two
implementations -- one in `server/src/tools/skill.rs`, one in `cli/src/commands.rs`
-- each documented as "the same roots in the same order as the other one". That comment
is the bug: a third caller appeared (`skill`), and a third copy would have been the
point where the three orders stopped agreeing.

## Bucket

`Bucket::ALL` is the precedence, and it is the only place the order is written. `project`
is the checkout the session works in, `harness` is what the pin installed under the home,
`general` is the machine's, which on this harness is the community clone linked into
`~/.agents/skills`. The words are the ones `skill` already prints and the prompts
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
what `@skill:` and `skill <name>` read, and the rest is what `skill ls` reports as
shadowed. A name in two layers is not an error -- the specification says the more
specific layer wins and asks the implementation to say when it did -- but it is also not
something to hide from the reader who typed the name.

## Skill

`entry` is resolved against the skill's own directory at read time, not at call time, so
a name read off the table is a file this process can execute. `dir()` is the anchor the
prose's `SKILL_DIR` stands for: the two must be the same directory, or the body and the
launcher disagree about where the script is.

## frontmatter

`---`, then top-level `key: value` lines, then `---`. Two of the keys are this harness's:
`description` (the table's line) and `name` (what `@skill:` resolves). Everything else in
the header is the reader's or another harness's.

A folded or quoted description on one line is read as written; multi-line YAML values are
not attempted, and that is a shape of the input rather than a limitation worth a parser: a
description that needs several lines is a body.

## metadata.entry

The one field this harness adds, and it goes where the specification allows an extension:
`metadata` is "an arbitrary key-value mapping", so a strict validator (`skills-ref`) sees a
skill that declares extra metadata rather than a file with an unknown key. `entry` names
the program the skill's body tells a reader to run -- `transcript`, `show`, `remote` -- and
naming it once in the header is what lets `gongga-cli skill <name>` find it without a
convention about which file in the directory is the program.

An indented line belongs to the top-level key above it: that is the whole of the nested
reading, and it is enough for `metadata:` / `entry:`. A skill that declares no entry is a
body and nothing to run, which is a real kind of skill (`monitor`, `self-control`) and not
an omission to guess around: the launcher says so and names the `SKILL.md` to read.

## shell_path

A path spelled the way a POSIX shell reads it, `C:\a\b` as `/c/a/b`. It started life in
`server/src/tools/bash.rs` as `msys_path`, where it quotes a path into a shell command, and
the launcher needs exactly the same spelling to hand a script to `bash`. Two copies of an
eight-line string function would drift in the one case that matters -- a drive letter in
lower case -- so the launcher and the server's shell quoting call this one.
