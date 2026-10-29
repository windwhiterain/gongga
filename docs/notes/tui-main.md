# tui/main.rs

## main.rs

`gongga-tui`: the window you actually run.

Two shapes, one command line: with a command it prints that command's events,
with none it opens the window. Driving the harness from a script or from the
agent's shell is no longer this binary's job -- that is `gongga`, the command
layer's own program -- but a person at a terminal can still ask for one command
and get its JSON with no UI in the way.

This file is the composition root of a window and nothing else. It names itself to
the
command layer, which is how `gongga-tui --help` describes the window rather than
something else, and hands the parsed line to the layer that knows what to do with
it -- the interactive loop for "no command", the command layer's one-shot runner
otherwise. It deliberately holds no command knowledge of its own: adding a
command must not mean touching this crate, which is what "transparent wrapper"
means here.
