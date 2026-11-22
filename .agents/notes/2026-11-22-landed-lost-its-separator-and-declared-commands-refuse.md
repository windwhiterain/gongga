# `landed` lost its separator, and every declared skill command refuses

2026-11-22, found in session `mask-offload` while diagnosing the relay errors. The same
turn's `read` call failed with a refusal that no source walk could explain at first:

```
file runs only as a Python call: make it a package -- … file/SKILL.md is the prose that
describes it.
```

The path in the refusal is BARE — and `~/.gongga/ship/skills/file` is a proper package.

## The chain, each step measured

- `space.rs::landed(None, path)` writes `remote://local` + path with **no `/` between them**
  (`format!("{PREFIX}{}{path}", local_machine())`), so a local Windows path becomes
  `remote://localC:\Users\17124\.gongga\ship\skills/file/SKILL.md` — exactly the shape
  `ls_skill` prints today. Its own invariant comment and `protocol::local_machine`'s doc
  both spell the local form **with** the slash (`remote://local/C:\x.png`), and the Python
  contract (`gongga_space.py`) parses `remote://local/<path>` and refuses a relative tail.
- `machine_of` splits at the **first** `/` — the one `read_layer` puts before the dir
  name — so the "machine" becomes `localC:\Users\17124\.gongga\ship\skills` and the tail
  becomes the bare relative `file/SKILL.md`. That is the refusal's path, byte for byte.
- `run_skill` takes the tail **without refusing the unconfigured peer name**
  (`machine_of(...).map(|(_, plain)| plain)`), so `is_package("file")` reads a path
  relative to the server's cwd and the package is refused.
- On POSIX the same arm loses the root slash (`remote://local/home/x` → tail `home/x`),
  so this is not Windows-only.

## Why it surfaced tonight

`454f542c` (21:14) deleted the native `read`/`write`/`edit` tool (`tools/file.rs`, -492)
and moved them to the content-declared commands, which resolve their skill through this
round-trip. The db agrees: 1223 ok / 1605 ok / 1348 ok calls all ran **before** the
22:40:55 restart (old binary, native tool); the **only** such call after the restart is
mask-offload's `read_3` — the first caller of the new door, and it refused. On the current
build every package-skill call through `run_skill` (read, write, edit, and any content
command) refuses on this machine.

## The fix (not yet applied — needs approval)

One line: `landed`'s local arm writes the separator, making it the same shape as the
peer arm — `format!("{PREFIX}{}/{path}", protocol::local_machine())`. Windows then spells
`remote://local/C:\…` and POSIX `remote://local//home/…`, both of which `machine_of`
and `gongga_space.py` already parse correctly. Stored references written in the old shape
were never parseable (`localC:\…` is an unconfigured peer), so nothing that worked gets
worse. A second, harder question to settle with the user: whether `run_skill` should also
refuse a path that names a machine no table configures, instead of quietly using its tail.

Gates for the fix: `mbx build`, `scripts/spawn_dir_selftest.sh` (its non-package refusal
must keep passing), and a real `read` call against a temp home.
