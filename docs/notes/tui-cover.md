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

## Terminal adaptation

No Sixel, Kitty, or iTerm graphics escape is emitted. An upper-half-block cell carries two
pixels, one as foreground and one as background, so the same ratatui/crossterm backend works in
terminals with and without a native image protocol.

`crossterm::style::available_color_count()` chooses RGB, the xterm 256-color palette, or the basic
ANSI palette. Indexed colors use the nearer of the color cube and grayscale ramp; basic colors
use the nearest of the sixteen named ANSI colors. `NO_COLOR` instead selects a monochrome ASCII
luminance ramp.

`crossterm::terminal::window_size()` supplies the physical cell aspect ratio where supported.
The source is center-cropped in physical units and then resized into the terminal's cell grid,
which preserves its aspect ratio instead of stretching it to the window. A conventional cell
twice as high as it is wide is the fallback for terminals that report no pixel dimensions. The
image scales both up and down, including very short or narrow windows.
