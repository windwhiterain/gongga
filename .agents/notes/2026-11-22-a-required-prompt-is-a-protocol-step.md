# A required prompt is a protocol step

The operator's verdict, after a TUI could not reach its server:

> 既然缺了字段，protocol当然要变啊

A name on `prompts::REQUIRED` is content this build cannot start without, so the content
that carries it is a new protocol -- not merely content published before the harness.
`81278dd1` added `memory_mask_notice` to `REQUIRED` and the content's `f8a908a` added
the template, and both kept protocol 14. That is the fault this records.

## What it cost

A home sat on a checkout from 2026-11-13 (`f428664`, protocol 14) while the published
content moved to a commit that carries the template. Every start:

```
prepare  -> pin = f428664          (the checkout's own `local` tip)
follow   -> starts the fetch, in a background thread
prompts  -> memory_mask_notice missing -> bail, 0.96s after the start
```

and the fetch died with the process. The next start read the same pin and refused again:
the home could not leave that state by itself. The TUI's client starts a server once, at
window start, and a later dial failure only reports `still trying`, so the window stayed
offline.

Two mechanisms were believed to cover this, and neither did:

- **Auto-fetch.** Real, but its answer is consumed by `refresh` *behind the port*
  (`lib.rs:680`), which a start that refuses never reaches.
- **The protocol wait.** Only fires when the checkout's history holds no commit speaking
  this build's protocol (`resolve` -> `wait_for_fetch`), and that wait could not fire
  either: `prepare` resolves at `lib.rs:244`, `follow` starts the fetch at `lib.rs:264`,
  so `fetched()` answered `the content fetch was never started` -- and `content()` cached
  that `Err` for the life of the process.

So the protocol step was right and the wait under it was missing.

## What changed

- Content: `ship/ship.toml` protocol 15, published first (`6b6ae46`).
- Build: `crates/server/ship.toml` protocol 15.
- `ship::start_fetch` is now the one place a fetch starts; `follow` is its ordinary
  caller and `wait_for_fetch` is the other, so a resolve that finds nothing readable
  makes the fetch instead of waiting for one nobody started.
- That fetch reads the repository from `config/config.toml` under the checkout -- the
  name the mapping has always had -- because the mapping that might name another file is
  inside the content this resolve cannot read. Asking `ship::config` there recursed into
  `resolve` and crashed the server (measured: four stack-overflow restarts, ten seconds
  apart, `walked=250` each time).

## Measured

`<worktree>/.dev/verify/` (script: `.dev/ship_protocol_check.sh`): a home cloned at the
newest protocol-14 commit and committed as `local`, then the 15 build started against it:

```
before 4e65e513 + local   (protocol 14)
PASS · 20s · after a74c1d0f · protocol 15 · listening 127.0.0.1:55822
```

The start fetched, fast-forwarded `main`, rebased `local`, and listened. The same build
with the recursive `config()` call never listened at all.
