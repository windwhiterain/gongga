# `--home` takes the environment over

`dff4b0e1`. Found by accident: an isolated run was started with `--home` on a scratch
home while the shell exported `GONGGA_HOME` pointing at the operator's home. The
server's own files went to the scratch home; **two channel directories and their
relays went to the operator's home.**

## Why one flag was not enough

Three different things resolve a home, and only the first read `--home`:

| who | how it resolves |
| --- | --- |
| the server | `Config::load_with_home` — `--home`, else `GONGGA_HOME`, else `~/.gongga` |
| a relay it starts | `config::home_dir()` — `GONGGA_HOME` first |
| a peer's runner | `config::home_dir()` — `GONGGA_HOME` first |

So a stale `GONGGA_HOME` in the environment overrode `--home` for everything the
server started, while the server itself honoured the flag. `Channels::for_home` builds
its root from `cfg.home`, but a channel placed on a peer is placed by the *peer's*
runner, which resolves its own home from the environment.

## The change

`server::main` sets `GONGGA_HOME` to the home it resolved, once, **before** the tokio
runtime is built — the only moment in the process's life when no other thread can be
reading the environment. `--home` absent changes nothing: the environment is already
what `home_dir()` would return.

## Evidence

Same script, `.worktrees/fix2/.dev/home-adopt.sh`: a decoy home exported as
`GONGGA_HOME`, the real one passed as `--home`, one session on a `Direct` peer with one
channel started there.

- before (`6f92b681` and earlier): the channel directories appeared under the exported
  home — the operator's, in the run that found this
- after (`dff4b0e1`): `channel directories under --home: 1`, `under decoy: 0`

## Left alone

`bundle::home()` reads `GONGGA_HOME` before anything else, and `bundle-main.md` argues
for that: a one-shot `gongga-cli` run from one home must not retire another home's
server. That is a safety invariant, so the bundle is untouched; the server now makes
the environment agree with the home it was given, which is what fixes the leak.
