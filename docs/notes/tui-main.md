# tui/main.rs

## main.rs

`gongga-tui`: the window you actually run.

Two shapes, one command line: with a command it belongs to the command layer, and
with none the name it was started under decides (`named`, below). A command line
goes straight to `cli::main`, which knows that `serve` takes the process and that
everything else needs a link; keeping that decision here would mean this binary
knowing which of the layer's commands needs its own runtime.

What is left here is the bare invocation under the window's name: a runtime, and
the window. It names itself to the command layer, which is how `gongga-tui --help`
describes the window rather than something else, and it holds no command knowledge
of its own -- adding a command must not mean touching this crate, which is what
"transparent cli wrapper" means.

`--client_only` is the one thing this front-end does not share with `gongga`: a
window starts a server when nothing answers (SPEC), and a person who already runs
one somewhere else needs a way to say so. It is declared on `cli::Args` because
both programs parse one `Args`, but only this one has something to suppress.

## named — one program, two names

A user may have exactly one file: this one. It carries the command layer, so it
has to be able to *be* the command layer -- the agent's shells need a `gongga` to
drive the server with, and there is nothing else to install.

Which of the two programs it is comes from argv[0], not from a flag: started as
`gongga` (or `gongga.exe`) it is the command layer, and the bare invocation lists
the catalog exactly as the cli's own binary would; started as anything else,
notably `gongga-tui`, the bare invocation is the window. A shell that typed
`gongga` must never be handed a screen it cannot see, and someone who starts the
window must not have to type a command first -- the name says which of the two was
meant, and it is the only thing that can, because both cases are "no arguments".

This is what `cli::server::install_command` relies on: given a program whose name
is not the command's, it installs that program *under* the command's name -- the
same file, second name (see `cli-server.md`).

The stem is compared case-insensitively, without the extension: `gongga.EXE` on a
case-insensitive filesystem is the same program, and `gongga-tui.exe` must not be
mistaken for it by a prefix.

## The window's entry point

`gongga_tui::main` builds the runtime and hands it to `interactive`; it lives in
the library rather than in this file so that the composition root stays a
dispatch and nothing else.
