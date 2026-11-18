# webui/server.rs

Two protocols, one listener: plain HTTP for the page's files, a WebSocket upgrade at
`/ws` for the pipe's frames. `hyper` (1.x) serves the connection and hands the upgraded
socket over; `tokio-tungstenite` -- already in the workspace for the IM bridge -- turns
it into a WebSocket. `hyper_util::rt::TokioIo` is the adapter between the two.

Each WebSocket text message is one `RenderMessage` exactly as `gongga-client` wrote
it; each message the page sends is one `UiIntent` exactly as the client reads it. The
host parses neither: it relays JSON Lines and nothing else, so it cannot drift from
`crates/client/src/render_wire.rs`.

## One window

`Slot` holds one page. A second browser tab that connects while a page is attached is
answered with a WebSocket close whose reason says so; the page shows that reason. A slot
per process rather than a page per tab is what makes "one client, one window" true on
this side too. Two windows are two processes with `--instance` names.

`Slot` also carries the shutdown watch: the process leaving is a message to the attached
window, not a process kill.

## The origin rule

The page can drive the harness, so the browser must be the only thing that can reach
the socket. Two checks on the upgrade, both cheap:

- `Host` must be a loopback name with this process's port (`127.0.0.1`,
  `localhost`, `[::1]`). A name that resolves to loopback but is not one -- a
  rebound domain -- is refused.
- when the browser sends `Origin` (it always does for a WebSocket), it must be
  `http://` and the same loopback name. Another site's page therefore cannot open a
  socket to a locally running host; without this, any page the operator visits could
  drive their harness.

Static files are served without either check: they are public source, and reading them
is not driving anything.

## No cache, and one failure channel

Files are served with `Cache-Control: no-store`: the page is embedded in this binary,
so a rebuilt host must be a new page, not a stale one. `/favicon.ico` answers 204 so a
browser does not log a 404.

The only way the page learns *why* it was refused or closed is the WebSocket close
frame: code `4001` and a reason cut to what a close frame carries. The host never
sends a frame the wire does not define.
