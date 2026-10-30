# tui/program.rs

## program

The window is installed twice under two names, so the one thing `main` cannot know
on its own is which half it is: `describe` asks the name, `took_layer` takes the
word `cli` off a command line. Both used to live in `main.rs`, where the only way
to test them was a second test binary -- Cargo links one per target, and a `bin`
target links the whole stack to run four assertions about a `Vec<String>`.

They are here for that reason and for a second one: this is the only decision the
window makes that is not the command layer's, and a crate can test what it owns.

## took_layer, named

See `tui-main.md`: the word is the program's, not the layer's, and the name is the
only thing that can say which half is meant, because a link cannot carry an
argument.
