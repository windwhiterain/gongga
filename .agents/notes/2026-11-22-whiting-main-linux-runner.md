# whiting_main: a Linux x86_64 runner, built on the peer

2026-11-22. Endpoint `whting@192.168.1.3` (whiting-main, Ubuntu 24.04, 12 cores / 31 GB).
Measurements: `.dev/verify/d569c1f56435-remote-whiting-main.json`.

## Route chosen: build on the peer, not cross-compile

The `remote` skill's `install` builds a Windows bundle, which cannot run on Linux, and it
does not cross-compile. Two ways out were probed before choosing:

- **(a) native build on the peer** — chosen. The peer reaches `index.crates.io` (200,
  0.28 s) and `static.crates.io` (200, 0.39 s) directly, and the tuna rustup mirror
  (200, 0.32 s) versus `static.rust-lang.org` (200 but 10.4 s). GitHub is unreachable
  there, so the source travelled as a `git archive` over SFTP (4.99 MB) instead of a clone.
- **(b) cross-compile from Windows** — rejected. No `zig`, no `cargo-zigbuild`, and only
  `x86_64-pc-windows-msvc` installed; it would need a linker and a sysroot that (a) does not.

The build needs no system packages: `reqwest` is `rustls-tls` (no OpenSSL) and `rusqlite`
is `bundled` (compiles SQLite with gcc). Rust 1.100.0 minimal, target
`x86_64-unknown-linux-gnu`, installed from tuna without touching the peer's dotfiles.

`GONGGA_BUILD=d569c1f56435` was set for the build: the archive carries no `.git`, and
`crates/protocol/build.rs` reads that variable first, so the programs report the exact
commit they came from rather than `unknown`.

**mbx has no Linux build** (it ships one Windows `mbx.exe`, 1.30.0), so the peer's compiler
is wrapped by nothing. Nothing was built on the Windows machine, inside or outside mbx.

## What is installed

    /home/whting/.gongga/bin/gongga-d45fe4033c0369c5   (30388440 bytes, mode 755)
    sha256 d45fe4033c0369c5324b2870090c73aa26fb5b30e87df178532c7777eb20612b

Release, stripped ELF x86-64, built in 81 s. `payload` is a default feature of
`gongga-bundle`, so it embeds all five programs; `gongga-runner` is a second bin of
`gongga-server`. Running it extracted `~/.gongga/payload/55fd5abc83e5a289/` with the
`.payload` marker, which is the proof the distribution is whole.

Protocol compatibility was checked against the **running** server (`6d9f27c59126-dirty`,
an ancestor of the source commit): `crates/protocol/` and `crates/server/src/remote/`
are byte-identical between the two, and the runner protocol is 5 on both sides. The
client/server `PROTOCOL_VERSION` (4) is a different number and is not the one a runner
greets with.

## The skill bug that blocks `install` and `verify` on a POSIX peer

`ship/skills/remote/_remote.py::peer_home` (line 200) reads the peer home with

    checked(peer.ssh(f"{BASH} -c 'pwd -W'", timeout=20), "reading the peer home")

`pwd -W` is a Git Bash builtin. On Linux it exits 1, and `checked` raises on a non-zero
exit — so the POSIX `pwd` fallback written immediately below it is **unreachable**. Both
operations die with:

    reading the peer home: bash: line 1: pwd: -W: invalid option

`install` (callers at 283 and 330) and `verify` (via `runner_verify`, 534) are blocked.
`check`, `configure`, `list_remotes` and `remove` never call `peer_home` and work fine.

Fix: do not wrap the `pwd -W` probe in `checked`; read its stdout only when the exit is 0
and otherwise fall through to the POSIX branch. **Not published** — this note records it,
the content change is a separate decision.

Workaround used, which reaches the same end state: place the distribution by hand at the
exact content-addressed path `install` computes (`sha256[:16]`, temp upload then `mv -f`,
chmod 755), prove the greeting with `check`, and write the table with `configure --write`.

A second, smaller defect: `probe()` kills the local `ssh` process but the peer's runner
survives it. `check` and `list_remotes --check` each left one behind (pid 10709, from this
distribution's own payload stamp); both were cleaned by pid. A probe that terminates only
the local end does not prove the peer's process is gone — which is what `verify`'s
busy-close step exists to check, and `verify` is the operation the `pwd -W` bug blocks.

## State and what remains

`~/.agents/remotes.toml` now holds `[remotes.whiting_main]` with host `whting@192.168.1.3`,
identity `C:/Users/17124/.ssh/id_ed25519`, and the absolute runner path above.
`[remotes.lichen-vm]` is unchanged.

`check` answers `{"result":"connected","peer":"whting@192.168.1.3","runner":"...","protocol":5}`.
`list_remotes --check` reports `connected: true, greeting: true, protocol: 5` and
`server.usable: null` with `stale_config: true`.

**Remaining: restart the server.** It reads `remotes.toml` at startup, so the new remote is
invisible to it until then; a restart kills the turn that asks for it, so it is left to the
user to coordinate. Nothing was cancelled or restarted, and no bundle was pointed at the
live home.

The peer build tree stays at `~/gongga-runner-build` (846 MB target) for a rebuild; it is
removable. `~/kv-mask` was not touched.
