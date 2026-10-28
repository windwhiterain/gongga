# `crates/server/src/prompts.rs`

## module doc

Every word this server hands a model as prose -- the system instructions and the
few notes it injects around a transcript -- is a string template in a config file.
The alternative was the status quo: eight string literals spread over four
modules, each one reachable only by recompiling, and each edit invisible to
whoever was actually reading what the agent said.

The templates live in `crates/server/default.config.toml`, which is the file a
new home is created from. So the defaults are not a fallback hidden in code and
the thing the user edits somewhere else: they are one file, read twice -- once
compiled in, once copied into the home.

## SHIPPED, TABLE

`include_str!` rather than a path resolved at runtime: a home's `config.toml`
can be damaged, deleted, or never have contained `[prompts]`, and a server that
cannot start because its prose is missing is a server that cannot answer at all.
The compiled-in copy is what "no opinion configured" means.

## Stamp, Stamp::of

The reload rule is a file size and a modification time, not a hash. A prompt edit
is a human typing into a file once, and the cost of missing it is one request
with the old wording; the cost of hashing the config before every model request
is paid on every request forever. The same argument is why `scan_fingerprint` in
`script.rs` compares mtimes.

`Prompts::templates()` keeps one snapshot behind a mutex and compares the stamp
first, so a steady server re-reads nothing. The stamp is `Option`: a missing file
is a state, not an error, and "still missing" is a match.

## read_table

Returns the templates *and* the problems it found, and a file that does not parse
yields nothing at all -- the shipped defaults stand. This is the same rule the
config loader already applies to a hand-edited `cli.toml`: a broken file is damage,
not a reason to refuse to run, and refusing would take the agent's tools away
because of a comma.

A non-string value (`size_hint = 7`) is reported and skipped, not fatal: one
mistaken key should not delete the other eight templates the home did write.

## templates() -- the warning about an unknown name

A name the code never asks for is almost always a misspelling, and a misspelled
template is invisible: the server keeps running, the old wording keeps being sent,
and nobody reads a log line that says nothing happened. Saying it once per read is
loud enough without repeating it every step.

## substitute

`{{name}}` and nothing else: no conditionals, no loops, no filters. A prompt that
needs a conditional is a prompt with two shapes, and two shapes are two templates
named by the code -- which is what `context_notice` plus `folded_none` and
`folded_through` do. A real template language would buy those two cases with a
dependency and a second syntax to learn about the harness.

## substitute -- the # Invariant

An unsupplied placeholder stays in the text. Deleting it would take the sentence
with it: a notice that lost its numbers reads as "you are near the limit", which
is exactly the message this project already rejected as unactionable. Leaving
`{{session}}` visible in what the model reads makes the bug obvious in the
transcript, where a silently shortened sentence never would be.

## install, shared

A process global, because the prose is wanted at layers that carry no config:
`store.rs` needs the interrupted-call line while building history, `tools/bash.rs`
needs the cap notice while spilling output, `config.rs` needs the refusal while
explaining a modality. Threading a `&Prompts` through all three means a `Store`
that can be opened only with a config, which is a larger lie about what a store
needs. `Prompts::defaults()` is the answer for a test or a tool with no home, so
the global is never the only way to get a template.

## constitution

Two files, in order: the machine's `~/.agents/agents.md`, then `AGENTS.md` beside
where the session works. The first is this machine's rules for every project, the
second the project's rules for this directory -- the pair `AGENTS.md` means in
practice, and the reason the session's file comes last is that the nearer rule is
the one a reader expects to win.

Read from the *reported* cwd, not the configured one: the point of the shell
keeping its directory is that the agent has moved, and a rules file found where
the session started would be the wrong directory's file.

Injected into `instructions`, never stored. A transcript row is what somebody
said; these rules are what the server reads off disk for this request, and
writing them into history would make an edit to a file look like a statement the
user made three turns ago -- and would be the one way this module could damage
history rather than leave it alone.

## starter

Splits at the `[prompts]` header and fills only what is above it. Everything
below is appended byte-for-byte, because a placeholder in a template belongs to a
later request: filling `{{model}}` at startup would freeze the image refusal to
whatever model the server happened to begin on, and `{{session}}` to nothing.

`Value::Bare` exists because TOML has types and this file has to be valid before
it is filled: every value is written quoted so the shipped file parses, and a
number or a bool loses its quotes on the way in. `store = "false"` would read as a
string to the config parser, which is worse than a literal that looks odd here.

## tests::the_starter_fills_values_and_leaves_the_templates_alone

Pins both halves of the split rule in one assertion pair: a value filled, a
template untouched. Without it the easy bug is a renderer that fills the whole
file, which produces a server that works perfectly until somebody changes model
and then sends a year-old model name inside the refusal text.
