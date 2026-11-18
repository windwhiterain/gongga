# client/child.rs

One place starts `gongga-client` for whichever front-end asked: the terminal
(`crates/tui/src/pipe.rs`) and the browser (`crates/webui/src/window.rs`) differ
in every other respect, and must not differ in how they start the child or where
its diagnostics go.

## The program is beside the window

`gongga-client` is found at the front-end's own directory, not on `PATH` and not by
searching a build tree: a bundle extracts the pair together, and a checkout runs the
pair cargo put in `target/debug`. A front-end that could not find it says so, naming
the executable it looked beside.

## The child's flags are the front-end's, minus the words

`--parent` is the front-end's pid, so the server the client starts follows the
*window*: the window outliving a crashed client keeps its server, and the window
leaving stops it. The connection flags travel only when this process has them;
nothing that names a command line travels to a process that only speaks the pipe.

## stderr is a file, never the pipe

The child's stdout is the frame stream and must stay clean. Its stderr is appended to
`<client-home>/client.log`: a parent that pipes stderr and never drains it fills the
pipe and blocks the child's loop, and a front-end that has no screen has nowhere else
to put the words. The browser front-end reads that file's tail when the child ends
without a frame, so the page can say what went wrong.

## Lifetime

The child is spawned with `kill_on_drop`, and closing the front-end's stdin is what
asks it to leave. Neither side polls the other's pid: EOF on stdin is the window
going away, EOF on stdout is the client going away. See `client-pipe.md`.
