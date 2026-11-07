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
before the event stream takes stdin, because that is where the answer arrives. Sixel and kitty are
answers to that question -- but an answer that arrives through a Chromium shell describes the pty
under it, not the painter above it. VS Code's terminal answers Device Attributes with attribute 4,
sixel, and paints no sixel; taken at its word, the whole picture went out as one escape that was
discarded, and the cover was black with a hint on it. Measured 2026-11-07 from that terminal:
`\x1b[?61;4c\x1b[6;18;8t\x1b[0n` -- sixel, and a cell of eight by eighteen. Put the same bytes in a
pseudo-console whose host never reads or answers and conhost replies `\x1b[?1;0c\x1b[0n` by itself.
An answer says what the layer that caught the question can carry; only a name says who paints.

So the name decides first, from a table ported out of `supports-terminal-graphics` and kept here:
the table is data that goes stale, and correcting one row should not wait for someone's release.

- Half blocks: `vscode`, `Tabby`, `Hyper`, `Bobcat`, and `Apple_Terminal`, which paints no picture
  protocol at all. A Device Attributes answer out of one of those is the pty's.
- Kitty: `kitty`, `ghostty`, `WezTerm`, `rio`, `WarpTerminal`, and the variables that survive tmux
  and ssh -- `KITTY_WINDOW_ID`, `KITTY_PID`, `GHOSTTY_RESOURCES_DIR`, `WEZTERM_PANE`,
  `WEZTERM_UNIX_SOCKET`, `TERM=xterm-kitty`, `TERM=xterm-ghostty`.
- iTerm2: `iTerm.app`, `mintty`, `LC_TERMINAL=iTerm2`.
- Sixel: `KONSOLE_VERSION`, `mlterm`, and `Windows_Terminal` from version 1.22, which is where
  sixel arrived there; an older build is cells.

A terminal the table does not name is left to the query, which can still say sixel or kitty. What
the answer measured is kept whatever the protocol becomes: the cell above is a real one, and it is
what the picture's rectangle is counted from. Only a reply whose number is not a cell is dropped
along with the protocol.

Writing a picture is not the same as painting one. Measured 2026-11-07: the cover leaves this
process as 863,069 bytes carrying one `\x1bP`, and a pseudo-console whose host carries no sixel
keeps every one of them -- the layer in between forwards what it forwards. That is the black page
again by another route, so a terminal is only named for sixel where the protocol reaches the
screen: Windows Terminal from 1.22, Konsole, mlterm.

iTerm2 is never taken from the query: the crate reads it off `TERM_PROGRAM`, and its list includes
`vscode`, `Tabby`, `Hyper`, `rio`, `Bobcat` and `WarpTerminal` -- shells that carry cells and no
pictures. A geometry with a side of zero, more pixels than a cell can be, or a width larger than its
height is not believed either: the protocol stands and the crate's own cell (ten by twenty) is used
in its place.

The protocol encodes the picture once per size and keeps it, so a repaint at an unchanged size
re-sends the escape it already built rather than encoding again.

## The frame that is owed

The cover is drawn before the first event, which is also before the terminal has necessarily
answered how large it is: `Cover::draw` paints nothing into an empty area, and a window that
draws one frame and waits would keep a black page until a key. So the cover remembers the area
it has been painted in, and a frame is owed while that is not the terminal's current size --
the first real one comes a moment later, without anything else having to happen.

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
