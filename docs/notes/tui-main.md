# tui/main.rs

## main.rs

`gongga-tui`: the window you actually run.

Two shapes, one command line: with a command it belongs to the command layer, and
with none the *name* decides -- this program is also installed as `gongga`, and
under that name the bare invocation is the catalog (`named`, below). A command
line goes straight to `cli::main`, which knows that `serve` takes the process and
that everything else needs a link; keeping that decision here would mean this
binary knowing which of the layer's commands needs its own runtime.

What is left here is a runtime and the window, and no command knowledge of its
own -- adding a command must not mean touching this crate, which is what
"transparent cli wrapper" means.

`--client_only` is the one thing this front-end does not share with `gongga`: a
window starts a server when nothing answers (SPEC), and a person who already runs
one somewhere else needs a way to say so. It is declared on `cli::Args` because
both programs parse one `Args`, but only this one has something to suppress.

## took_layer — `gongga-tui cli ...`

This program carries the command layer, so it has to be able to hand a command
line to it even under its own name. `cli` is the word that says which half is
meant:

```sh
gongga-tui            # the window
gongga-tui cli        # the catalog, exactly like `gongga` with nothing at all
gongga-tui cli get_messages --session main --range -5:
```

The word is taken off the front of the command and the rest is the layer's.

It belongs to the program, not to the layer: it is not in the catalog, the server
has never heard of it, and it is answered before anything is dialled -- an empty
command line after it means "list what the server publishes", which is what an
empty command line means for `gongga` too.

A command line that never says it is still passed straight to the layer, because
this front-end is a transparent wrapper: `gongga-tui serve --port <p>` is how the
window brings its own server up, and renaming that would mean this crate knowing
which commands exist.

## named — why the word is not always needed

The server puts *this program* into the home's `bin` under the name `gongga`
(`cli-server.md`), so one file has two names and two meanings:

| started as | bare invocation |
|---|---|
| `gongga-tui` | the window |
| `gongga` (a link to it) | the catalog |

The name, not a flag: a link cannot carry an argument, so the name is the only
thing left that can say which half is meant. It is compared as a stem and
case-insensitively, so `gongga.exe` and `GONGGA.EXE` are the same command, on
every platform and whatever directory the file sits in.

`cli` still works and still means the layer, because whoever typed it means it.
Nothing else changes: a command line is the layer's whatever the name is.

## The window's entry point

`gongga_tui::main` builds the runtime and hands it to `interactive`; it lives in
the library rather than in this file so that the composition root stays a
dispatch and nothing else.
