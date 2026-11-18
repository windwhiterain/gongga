# webui/main.rs

`crates/webui` is the second front-end after the terminal: a Cargo member whose
binary, `gongga-webui`, serves a page on loopback and speaks the client's pipe. It
is not in the bundle, by the 2026-11-07 verdict that only the terminal ships and other
UIs distribute themselves; a bundle would carry a TCP listener no terminal user asked
for.

The process builds nothing and reads no state: it parses the command line, binds the
page's address, prints the URL, and serves until asked to leave. Ctrl+C takes the one
window's page and the client child with it -- a page asked to leave releases the child,
whose own exit is what cancels a server this process started. Killing the host with
the page attached would leave a client without its pipe and a server nobody owns.

What a page is, and why one at a time, is `webui-window.md`. The listener and the
origin rule are `webui-server.md`.

## Why a page instead of another desktop window

The Avalonia front-end that preceded this one proved the contract was real: a full
window over the pipe, in another language, with no Rust change. It paid a second
toolchain for it: a .NET 10 SDK, a XAML source generator with its own Roslyn floor,
and a 120 KB project that only that SDK can build. The browser asks for none of that
on the reading side -- and the one thing it still needs, a local process to hold the
pipe, is this crate.

The trade the page makes is one connection: the browser is not a child of the client,
so the host is the parent, and a page reload is a new child rather than a resumed one.
That is what `webui-window.md` explains.
