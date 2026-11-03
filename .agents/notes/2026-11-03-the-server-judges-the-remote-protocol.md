# The server judges the remote's protocol, the script asks

2026-11-03: the peer `lichen-vm` was one protocol behind. The server spoke protocol 2
(`crates/server/src/remote/protocol.rs`, raised by 444fa7f because `Open` gained `args`);
the runner installed on the peer greeted with protocol 1, so every shell on that endpoint
was refused at the greeting.

## What was wrong with the checking

`ship/skills/remote/remote.py` held `PROTOCOL = 1` -- a second copy of a number the server
owns. It had already drifted, and it drifted in the direction that hid the fault:

- `list --check` reported `connected: true` for a runner the server refuses, because it
  asked "does the peer answer?" and not "can this server speak to it?";
- `install` probed the new distribution and compared the greeting to its own constant, so
  installing the fix would have failed on the last step -- the skill could not repair the
  fault it was written to repair.

## The shape it has now

The server is the one that speaks the protocol, so it is the one that decides. The script
reads what a runner says and asks the server for the verdict (`gongga-cli list_paths
--remote <name>`, which opens the endpoint and answers). `usable` is true, false with both
versions named, or null when the question could not be answered -- a null is not a verdict.

The script holds no protocol number any more, so it cannot disagree with the server. It
also writes the remote's `runner` line itself (`install --write`), changing nothing else in
the table and refusing a remote that names another machine. The remotes file is read at
server startup, so a runner written there is not in use until the server restarts; the
check says so when the file is newer than the server.

The server's own refusal now carries the way out: the endpoint knows its host, port and
key, so it names the command that replaces the distribution on the peer.

## Verified

- The peer carries the distribution the running server was built from
  (`gongga-4288a3f33b93e01d.exe`, payload stamp `e5ae94cb9213d9b5`): `list_paths --remote
  lichen-vm --path .` answers `88 entries`, and `remote list --check` answers
  `"server": {"asked": true, "usable": true}`.
- The old peer was protocol 1 and the refusal named both numbers, measured through the
  running server (`the endpoint's runner speaks protocol 1, and this server speaks
  protocol 2`).

## Open

- The check reaches the server through `list_paths`, because that is the command that opens
  an endpoint today. A command of its own would answer the same thing without the listing.
- The peer is re-installed by hand when its protocol moves. The install is one command now;
  the upload is not automatic, and should not be: it writes to another machine.
