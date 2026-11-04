# A window owns only the server it started (WIP)

User's report, 2026-11-04: the TUI was connected to a server started on its own,
and closing the window cancelled that server. The rule the user stated: **the TUI
may only stop a server it owns -- the one it spawned, or a handover of that one --
and the header has to say whether it is own.**

## What was wrong

`tui::run::dial_now` sent this window's pid in every hello (`Endpoint::owning` ->
`ClientInfo::owner`), and `server::net::authenticate` turned that into
`signal::adopt`: the first client to connect to an *unowned* server made itself its
owner, and the server followed that pid -- its death was a stop request. So every
server somebody started by hand (`gongga serve`, the bundle's `server`, a command's
`ensure`) died with the first window that read it.

The claim existed for one case: `evolve`'s successor arrives already running, and
something had to keep it the window's. That case is already carried by `--parent`:
`evolve.sh` copies the old server's argv, so the successor names the same window.
The claim was therefore redundant *and* harmful.

## What changed

- `dial_now` sends no owner; a dial is a read. `Endpoint::owning`, `ClientInfo.owner`,
  `signal::adopt`/`claim` and the handshake call are gone.
- Ownership is `--parent`, decided before the first connection: the pid `spawn`
  answered, or the pid the server follows being this window's own (a handover).
  One answer, `Server::is_ours`, decides both the header and the way out.
- Header: `own #pid`, `own #pid (handover)`, `not own #pid (another window)`,
  `not own #pid (no owner)`.
- `Link::cancel` now waits for the server's `CmdResult`: `send` only queues, and a
  window one statement from dropping its runtime could ask for a stop that never
  left the process. Found by the check below, not by reading.

## Measured (`.dev/verify/ownership.sh`, real processes, temp home, kernel port)

| scenario | before (`45de7ae` + the other agent's `af7e7fc`) | after |
| --- | --- | --- |
| server started by hand, `--client_only` window | **server died**, header `adopted #39100` | server alive, header `not own #39100 (no owner)` |
| window starts its own server | server stopped (header `own`) | server stopped (header `own`) |
| server arrives with `--parent <window pid>` (handover) | header `adopted #32620`, window's cancel never arrived | header `own #32620 (handover)`, cancel arrived and the server stopped |

New build: `PASS · 33s · ownership-new · 45de7ae`. Old build:
`FAIL · 137s · ownership-old · 4 check(s)`.

## Left

- The plain `gongga tui` still takes the address on the way in: it cancels whatever
  serves this home and starts its own. That is SPEC's `bundle server` and
  `88482ef`'s design, and it is a start-up takeover, not a close-down one. Asked
  the user whether that should join instead.
- Verification harness lives in `.dev/verify/ownership.sh` (gitignored, not a
  repo test): no tests were written, per the standing rule.
