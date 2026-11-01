# tui/program.rs

## program

The window is installed twice under two names, so the one thing `main` cannot know
on its own is which half it is: `describe` asks the name, `took_layer` takes the
word `cli` off a command line. Both live here rather than in `main.rs` because
this is the only decision the window makes that is not the command layer's.

## took_layer, named

See `tui-main.md`: the word is the program's, not the layer's, and the name is the
only thing that can say which half is meant, because a link cannot carry an
argument.
