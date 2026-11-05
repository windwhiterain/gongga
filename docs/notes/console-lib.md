# `crates/console/src/lib.rs`

One question: how does a program the harness starts keep its console off the
user's desktop?

## The mechanism, measured on this machine

A Windows program that has no console gives every console program it starts a
**new visible console** -- a window appears, titled with the child's path. That
is the whole of the user's complaint: the harness starts programs in the
background, and each one put a window on the desktop.

`CREATE_NO_WINDOW` (0x0800_0000) gives the child a console of its own that is
never shown. Measured here:

- a parent with no console: the child without the flag opens a visible window;
  the child with the flag opens none, and its own children inherit the invisible
  console and stay invisible;
- a parent with a console: the child without the flag inherits it, and the number
  of console hosts does not change.

So the flag is needed exactly where the parent has no console to hand down, and
the rule "the parent has none" is decidable (`GetConsoleProcessList` fails
without a console) at no cost.

## Why the flag is not on every child

The server is started with `CREATE_NO_WINDOW` already (`cli::server::detach`), so
the server holds an invisible console and every child it starts inherits it:
adding the flag there would be a step backwards, because a child with the flag
gets a console *of its own* -- Git Bash alone then adds three more console hosts
of its own. The same holds for the tools the server runs (`bash`, `python`,
`git`, the quota probe) and for the TUI's children, which run in the user's
terminal and inherit it.

The flag belongs to the programs that are started **from a parent that has no
console**:

| where | who starts it |
| --- | --- |
| `server::supervise` | the supervisor, itself console-less in the usual case |
| `server::remote::runner` | ssh, on the peer's machine |
| `cli::skill_command` | the command layer, on a pipeline |
| `bundle::payload_impl` | the distribution's head |

## Why `flags` and not the constant alone

The standard library's `creation_flags` **replaces** the flags; it does not add
to them. A caller that needs a flag of its own -- `CREATE_NEW_PROCESS_GROUP`
for the detached server, `CREATE_SUSPENDED` for the shell on the peer -- has to
compute the sum itself, or the second call silently drops the first.
`console::flags(extra)` is that sum, and `console::apply` is the same sum handed
to a standard library command.

Tokio's `creation_flags` takes the same number and has the same replacement
rule; the crate that drives a Tokio command uses `flags` directly.

## The bundle

`crates/bundle` is the head of a distribution and depends on nothing, on purpose:
it has to build on a machine that has nothing. It spells the flag itself, which
is the one place in the repository where the value appears twice.

## Measured on this machine

A probe started each program from a parent with no console at all
(`DETACHED_PROCESS`), sampled the visible console windows while it ran, and
compared the build of `cb7fc75` with the build that carries the flag:

| what was started | before | after |
| --- | --- | --- |
| the server, restarted by the supervisor | one window, titled with the exe's path | none |
| the command layer's Python probe | one window, titled `AppInstallerPythonRedirector.exe` | none |

The windows that were already there -- the reader's own terminals -- are in
both columns.

## What is left, and why

Running a skill's entry still shows a window when the command layer itself has
no console: the entry is meant to inherit the reader's terminal, and a flag
would give it a console of its own, invisible and unable to draw. The same
reason keeps `bundle::take_over` attached to the console it found. A program
that wants a windowless child has to give the child its own console -- and the
programs that start other programs here now say so.
