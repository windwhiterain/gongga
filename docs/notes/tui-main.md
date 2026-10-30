# tui/main.rs

## main.rs

`gongga-tui`: the window you actually run.

Two shapes, one command line: with a command it belongs to the command layer, and
with none it opens the window. A command line goes straight to `cli::main`, which
knows that `serve` takes the process and that everything else needs a link;
keeping that decision here would mean this binary knowing which of the layer's
commands needs its own runtime.

What is left here is the bare invocation: a runtime, and the window. It names
itself to the command layer, which is how `gongga-tui --help` describes the window
rather than something else, and it holds no command knowledge of its own -- adding
a command must not mean touching this crate, which is what "transparent cli
wrapper" means.

`--client_only` is the one thing this front-end does not share with `gongga`: a
window starts a server when nothing answers (SPEC), and a person who already runs
one somewhere else needs a way to say so. It is declared on `cli::Args` because
both programs parse one `Args`, but only this one has something to suppress.

## The window's entry point

`gongga_tui::main` builds the runtime and hands it to `interactive`; it lives in
the library rather than in this file so that the composition root stays a
dispatch and nothing else.
