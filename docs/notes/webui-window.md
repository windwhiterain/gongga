# webui/window.rs

A client's pipe has no replay: the frames between the child starting and a reader
attaching are gone. That is why a page is a window in the terminal's sense -- attach
spawns `gongga-client`, detach ends it -- and not a viewer of one long-lived child. A
reloaded page therefore gets a fresh client, which dials the server, reads its greeting
and refills its window from its journal on disk (`client-window-journal.md`); the
startup burst it would otherwise have missed is new again.

One page at a time: `Slot` in `webui-server.md` refuses a second.

## The pump

Three things can happen, and all three end the window:

- a line from the child's stdout is one frame on the page's socket;
- a text message from the page is one intent on the child's stdin, newline appended;
- this process is leaving (`Slot`'s shutdown watch).

When it ends, stdin is dropped -- that is the window leaving -- and a reader keeps
draining stdout until the child's writer exits. Without that drain the pipe can fill
after the window is gone and the child would wait forever inside a write; the TUI keeps
the same reader for the same reason. Then the child is waited for, which is what lets a
server it started be cancelled by its own exit. `kill_on_drop` is the last resort for a
child that outlives the code.

## The reason a page shows

The page is a stateless renderer: a close with no reason is a mystery to the operator,
and the host has the only text there is. When the client ends, the last line of
`<client-home>/client.log` becomes the close reason; a refusal (busy window, client that
could not start) carries its own words. The close code is `4001` -- the host's own
channel, outside the WebSocket-defined range.

Ping gets a pong; binary and other messages are ignored rather than breaking the
window. An unreadable frame is the client's problem and the host never sees it: it
relays text, so a bad line would reach the page, which skips it.

## stderr and the log

The child's stderr is already a file (`client-child.md`); this window reads only its
tail, at most the last 8 KiB, and only once the child is gone.
