# tui/examples/frame_cost.rs

## frame_cost.rs

What one frame costs as the log grows -- the regression check for the scrollback
rewrite.

`cargo run --release -p gongga-tui --example frame_cost`

The number that matters is that the "new" columns do not move when the log gets
longer. The "old" column is the pre-rewrite renderer kept here on purpose: it
wrapped every line and allocated a string per row, every frame.

## frame_of_two_panes

The first block of columns measures one window asked at one width, which a frame is
not: a frame asks the command area at the window's width and a session's pane at a
share of it. That second number is what caught the regression this file exists for,
in its second form -- with one width for the whole log, every frame laid the entire
log out twice, and the cost of a keystroke grew with the conversation:

```
            columns above    46ms/frame with 80 answers, 120x40
after lay_out per window    75us/frame
```

So the last line builds a log with two windows at two widths and times what a frame
asks of the log -- the session's rows, then the command line's. It must stay flat as
the log grows; if it starts tracking the line count, a width has found its way back
into the log.

## old_frame

The original measured a char by formatting it into a `String`, which is one
allocation per character of the whole log -- the heart of the bug. That also
means the old column measures UTF-8 bytes, not display cells: for a Chinese
character it charges 3 where the renderer draws 2. The corpus here is ASCII, so
the two columns are like for like, and the width bug is `Log::window`'s own
business rather than this example's.
