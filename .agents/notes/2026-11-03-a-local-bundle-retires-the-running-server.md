# A local bundle retires the running server

`gongga.exe` is not a program that runs; it is a bundle that *installs* a payload and then
takes the home. Every invocation places its embedded programs under
`<home>/payload/<stamp>/` and calls `payload::retire`, which stops every other build's
server (`crates/bundle/src/payload_impl.rs`).

Measured, 2026-11-03: a python probe started `target/release/gongga.exe runner` and
`target/debug/gongga.exe runner` to read what a runner greets. The debug build's stamp
differed from the running server's (`e5ae94cb9213d9b5`), so the retire step stopped the
server that was serving the session: the tool call came back "the turn was interrupted",
and the server was back 70 seconds later with the turn replayed (`resuming turns left
pending by the last run resumed=2`).

The rule for a machine that runs a server: do not start a bundle you built. To read what a
runner says, run the runner (`~/.gongga/bin/gongga-runner.exe`) -- it is the same payload
as the server and touches nothing. `gongga-runner.exe` is byte-identical to the one beside
the running server (`fnv 0x9379323a8101698e` on 2026-11-03), so its greeting is that
server's own answer.

The same rule covers the peer: `remote install` uploads a bundle to the peer and never runs
the local one, so building a bundle is safe; starting it is not.
