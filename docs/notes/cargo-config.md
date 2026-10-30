# .cargo/config.toml

## linker = rust-lld (Windows, MSVC)

`rust-lld` ships inside the rustup toolchain, so this asks nothing of the machine
and changes nothing about what is built: same crates, same flags, same
self-contained executables.

It is here for one measurement. `cargo test -p gongga-server --no-run` after a
one-line edit in that crate, same tree, back to back:

| linker | server harness (19 MB) | `gongga-tui` (4 MB) |
|---|---|---|
| MSVC `link.exe` | 5.1 s | 3.1 s |
| `rust-lld` | 2.7 s | 2.9 s |

Link time is the part of a dev loop a flag can still move; the rest is codegen of
the crate that changed. The 19 MB harness is where it showed, which is why this is
the linker and not a set of profile knobs.

## What was measured and dropped

- `-C prefer-dynamic` (std as a DLL): **no gain**. 3.4 s against 3.3 s for the
  same build, the server harness still 18.7 MB, and no `std-*.dll` in its imports
  -- the time is this workspace's own code, not std. It would also break the
  install: `cli::server::place` puts exactly one file in `<home>/bin`, and
  `scripts/dev.sh` stages one file into `.dev/`, so a build that needed DLLs
  beside it would not start as `gongga`.
- Workspace crates as `dylib`: the same install problem, multiplied by every
  crate, and it ties the binaries to one toolchain's Rust ABI.
- `[profile.dev] debug = "line-tables-only"`: 3.0 s against 3.3 s on top of lld --
  real, but it is a debugging experience traded for 300 ms, so it stays off.

## Scoping

`[target.'cfg(all(windows, target_env = "msvc"))']`, not the whole file: the gnu
toolchain on Windows has its own linker, and every other platform's default is
already the one to use.
