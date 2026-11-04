# The file tools reach the peer

User's instruction, 2026-11-04: `read_file`, `write_file` and `replace` were this
machine's alone -- they ran the Python driver locally even for a session whose
directory was on a remote, so a peer's file could only be reached by shipping a
`remote_python` program. Give them the remote, exposed as one optional `remote`
argument.

## The wheel that already existed

The three verbs are the Python driver with a structured request, and
`remote_python` already starts that same driver on the peer. So the remote half is
`RemoteRepl::spawn` with the same `DRIVER`, not a new protocol: no `Op::Write`, no
protocol bump, no peer reinstall. A peer without Python refuses the call, which is
the same answer `remote_python` gives.

The runner's file side (`Op::Stat/Read/List`) stays what it was -- the meta
expansion's read-only path. Writing through it would have meant a protocol version
and a reinstall of every peer for three verbs that already work.

## One machine, written out loud

`remote` takes the session's remote name, and `none` (or omitting it) is this
machine -- the same reading `crate::session::remote_request` gives a `--remote`
everywhere else. A second configured peer is refused, exactly as `remote_bash`'s
`target` is: a session's directory is on one peer, and a file tool has no directory
on another machine to be in. The refusal names `set_session --remote` as the way to
move.

## Whose file: `Tool::machine` takes the arguments

A tool's output is expanded on the machine that ran the tool, so a file read on the
peer must report the peer. `Tool::machine` now takes the call's arguments, and the
three file tools answer `Machine::Remote` when their `remote` names the session's
own. The default implementation still answers for where the session stands, so no
other tool changed.

## The selftest's `send_message` order

`scripts/remote_selftest.sh` had been red since `a40842bc` changed `send_message`'s
declaration order (`message` then `name`) and updated `set_session` but not the
five `send_message probe "..."` calls. They bound the session name as the message
and the message as a session, so every turn was a `no session named ...` the script
redirected to `/dev/null` and then waited 120s for. Fixed to the declared order.
