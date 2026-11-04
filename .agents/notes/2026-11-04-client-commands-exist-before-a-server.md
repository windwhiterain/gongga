# A window with no server is still a window (WIP)

User's report, 2026-11-04: with `--client_only` and no server up yet, nothing could
be typed -- every `/` line was refused and the completion popup was empty. The rule
the user stated: **client commands must always be enterable and completeable.**

## What was wrong

The catalog was the greeting's, whole: `Catalog::new()` was empty and
`set_commands` filled it at connect time. Offline, `command("connect_server")` found
nothing, so `submit_tokens` reached the "unknown command" branch; the offline guard
(`is_client_command`) also answered false for everything, and the popup had no
commands to offer. Wait -- the guard answered false, so the refusal was the offline
one: `connect_server: not connected to the server, nothing was sent`.

The client commands are not the server's to publish: SPEC fixes the six of them
(`connect_server`, `del_sticky`, `serve`, `set_render`, `set_render_session`,
`set_sticky`) and `protocol::client_commands()` is the one published list of them --
the same list `serve` parses against.

## What changed

`Catalog`'s `Inner` has two lists now: `published` (the greeting's, which wins for a
name both carry) and `client` (`protocol::client_commands()`, there from the first
frame). `command`, `commands`, the completion's command list and `session_named`
read both; `fills_sticky`/`sticky_names` still read only what the server published,
so a fresh home's `set_sticky session main` is still stored rather than refused
(nothing has proven the type wrong yet).

A *server* command offline is unchanged: no spec exists for it, so the line says
`not connected to the server, nothing was sent` rather than "unknown command".

## Measured (`.dev/verify/client-cmds.sh`, real TUI in a pseudo-console)

| check | before | after |
| --- | --- | --- |
| `/` with no server lists client commands | no popup | `connect_server` listed |
| `/set_render --bl` completes a flag | nothing | `--block_size` |
| `/set_render --block_size 4` runs | refused | `render block_size = 4, box_edge = 2` |
| `/connect_server --port 9711` runs | `not connected ... nothing was sent` | `server is 127.0.0.1:9711; reconnecting` |
| `/send_message ...` with no server | refused, says why | refused, says why |
| `/co` against a server | -- | `connect_server` |
| `/set_s` against a server | -- | `set_session` |

Before: `FAIL · 20s · client-cmds · 4 checks`; after: `PASS · 41s · client-cmds`
(7 checks). The ownership harness still passes at the same tree (`PASS · 26s`).

## Not done, on purpose

`set_sticky --type` offers no candidates offline: the types are the kinds the
server's arguments declare, and this client must not keep a second vocabulary for
them. The command works; only its suggestion waits for a greeting.
