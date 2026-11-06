# tui/cover.rs

## The startup frame

The root `cover.png` is embedded with `include_bytes!`. A bundled window therefore carries its
cover; startup does not depend on the working directory or on installing a second asset file.
The PNG is decoded once, and its decoded pixels are released when the cover closes.

`Tui::new` still clears the alternate screen before the first frame. The cover then paints every
cell rather than relying on ratatui's empty-cell diff, so neither an earlier process's alternate
screen nor a blank image cell can leave stale ink behind.

The cover has no timer and does not hide connection errors after an arbitrary delay. The first
key press or mouse gesture closes it without operating the window underneath; Alt+Q still quits.
A paste closes it and remains a paste. Resizing keeps the cover visible and redraws it at the new
size. The cursor is hidden for the cover and restored once on the transition to the window, not
on every repaint.

## The picture protocol

The cover is drawn as pixels, not as cells. `ratatui-image` asks the terminal which picture
protocol it answers -- sixel, kitty, or iTerm2 -- and draws in half blocks, two pixels per cell,
for the terminals that answer none. So the same build is sharp in a terminal that carries pixels
and still shows the picture in one that does not.

The question is put to the terminal once, at startup, after the alternate screen is entered and
before the event stream takes stdin, because that is where the answer arrives. Nothing here
overrides it: a terminal that reports a protocol gets that protocol, and one that reports nothing
is drawn to in half blocks.

The protocol encodes the picture once per size and keeps it, so a repaint at an unchanged size
re-sends the escape it already built rather than encoding again.

## What is drawn

The cell size the terminal reports decides the grid. The picture's rectangle is counted in cells
from the same font size the protocol uses for its own arithmetic, so the rectangle and the
encoded picture cannot disagree about how many cells it covers.

The picture is fitted whole: whichever of its width and height runs out first decides its size,
and the rest of the screen stays black. Nothing is cropped, so a window of any shape shows all
of the picture. The last row is reserved for `press any key` and carries no pixels, which is why
a protocol that repaints its own rectangle cannot paint over the hint.

Uncovered cells are painted black rather than left blank: a blank cell keeps the terminal's own
background, and that is not a colour this program chose. For the same reason the picture is
composited onto opaque black before it is encoded -- a protocol that pads its canvas leaves
whatever it did not paint, and a transparent pixel is not black.

The picture has no frame, so its rectangle would otherwise meet the black around it along a hard
line. Every pixel is multiplied by a smootherstep fade that reaches full brightness
`FEATHER_FRACTION` of the short side in, which leaves the edge itself black and the transition
invisible. The fade carries grain, strongest at the very edge and gone inside the picture: a
smooth ramp over a few dozen rows reads as a band, and speckle dissolving into black does not.
The grain comes from the pixel's own coordinates, so it is the same on every run.

The name is stamped into the picture's own pixels, not drawn as cells: a cell can only paint a
background, so letters written into cells would leave black boxes on the photograph. The letters
come from the `font8x8` bitmap font, white with a black outline, `WORDMARK_HEIGHT` of the
picture's height tall, ending at `WORDMARK_BASELINE` of it. Being part of the picture, the name
scales with it and needs no rows of its own.

Downsampling uses the crate's default nearest filter: a terminal cannot show more than its own
grid, and a smoother kernel would invent ringing around the edges of a picture being taken down
to a few dozen rows.
