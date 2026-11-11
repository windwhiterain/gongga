# prompt prose leaves the binary

Every sentence the server hands a model now comes from a `[prompts]` template: the
seventeen the config already had, plus seventeen that were still literals in
`explore.rs`, `tools/tunnel.rs`, `tools/detached.rs`, `commands.rs`, `agent.rs` and
`store.rs`. Code keeps the choice of template, the indentation of a list, and nothing
else. `prompts::REQUIRED` names all thirty-three, so a config that lacks one is a
refusal to start rather than an empty sentence in a prompt.

What is still prose in the binary, and deliberately so for now: the text a *tool*
answers with -- `ToolOutcome::err` in `tools/*.rs`, command outcomes in `commands.rs`,
the unknown-argument message in `tools/mod.rs`. That is roughly a thousand literals. It
is agent-visible, so the same argument applies to it, but it is a different change: each
message needs a name, and a name is content shape. Decide before starting it.

The coupling worth remembering: a name added to `REQUIRED` is a build that cannot start
on content that does not carry it. So content goes out first and the harness after.
Adding a name is safe for an older reader (it ignores keys it never asks for); renaming
one is not, which is why `monitor_error` kept its name through a rewrite of its text.
