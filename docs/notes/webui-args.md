# webui/args.rs

The command line is the client's own, augmented rather than re-declared: the program
asks `gongga_client::Args` for its clap command and adds the two flags a browser
window needs. `--server`, `--home`, `--client-home`, `--key`, `--client_only` and
`--force` therefore mean exactly what they mean for `gongga-client` and
`gongga-tui`, and stay one definition. A future connection flag is one edit, not
three.

A stray word is refused, not interpreted: a command line belongs to `gongga-cli`.

## `--bind` is loopback only

The page can drive the harness -- submit lines, delete sessions, quit the client. A
listener with no authentication must not answer another machine, so an address that is
not loopback is refused before anything binds. The default is `127.0.0.1:9701`, one
above the server's `9700`; the printed URL carries the real port, so `--bind
127.0.0.1:0` also works.

The same fact reaches the socket as an origin rule; see `webui-server.md`.

## `--instance`

One client owns one client home through `client.lock`, so two windows need two homes.
The terminal front-end gets its own by `--client-home`; a browser window that wants
"the same settings, a second copy" has `--instance <name>`, which maps to
`<client-home>/<name>`. The name must be one directory -- no separators, no `..`.
This is the client home the client already owns: the page adds no file of its own to
it.
