# tui/examples/frame_cost.rs

## frame_cost.rs

What one frame costs as the log grows -- the regression check for the scrollback
rewrite.

`cargo run --release -p gongga-tui --example frame_cost`

The number that matters is that the "new" columns do not move when the log gets
longer. The "old" column is the pre-rewrite renderer kept here on purpose: it
wrapped every line and allocated a string per row, every frame.

## old_frame

The original measured a char by formatting it into a `String`, which is one
allocation per character of the whole log -- the heart of the bug. That also
means the old column measures UTF-8 bytes, not display cells: for a Chinese
character it charges 3 where the renderer draws 2. The corpus here is ASCII, so
the two columns are like for like, and the width bug is `Log::window`'s own
business rather than this example's.
