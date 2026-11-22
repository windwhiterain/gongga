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

## The fix (landed)

One line: `landed`'s local arm writes the separator, making it the same shape as the
peer arm — `format!("{PREFIX}{}/{path}", protocol::local_machine())`. Windows then spells
`remote://local/C:\…` and POSIX `remote://local//home/…`, both of which `machine_of`
and `gongga_space.py` already parse correctly. Stored references written in the old shape
were never parseable (`localC:\…` is an unconfigured peer), so nothing that worked gets
worse. A second, harder question to settle with the user: whether `run_skill` should also
refuse a path that names a machine no table configures, instead of quietly using its tail.

Branch `fix/landed-separator` at `fea7c5e8`. Gates (`.dev/verify/fea7c5e8-landed-read.json`,
worktree-local): build/fmt/layout green; comments-check 109 violations, the set identical
to the d1d590c2 baseline; `spawn_dir_selftest.sh` — every skill scenario green (the
regression scenario first), scenario 6 still red as the documented Left-open of 454f542c
("a peer session with an omitted cwd now really reaches that peer"); scratch L3 on a
temp home — the declared `read` returns the file's lines, the refusal is gone, `ls_skill`
spells `remote://local/C:/…`. Two sessions wrote the same fix while the other build waited on the network:
`fix/landed-separator` (`fea7c5e8`, the separator alone) and `fix/skill-separator`
(`4d24e01b`, the same spelling with both arms unified into one `format!`, and the
invariant written above the function). One landed: `fix/skill-separator`, because the
unified arm leaves no second copy of the spelling to drift. `fix/landed-separator` is
superseded; its measurement stands and is cited here.

Gates on the landed sha (`.dev/verify/4d24e01b-landed-separator.json`): `mbx build` exit 0;
`mbx fmt --check` exit 0; `test-layout-check` 0 violations; `comments-check` 10 errors, the
same count as the `e928efaf` baseline and none in `space.rs`; `scripts/skill_contract.sh`
PASS; `scripts/skill_call_selftest.sh` PASS 6/6, whose package call through `run_skill` is
the regression own scenario; `scripts/spawn_dir_selftest.sh` every skill scenario green,
scenario 6 red as above. `scripts/meta_selftest.sh` is red on this sha **and on the
unpatched binary** ("a first turn wrote 0 prompt rows"): a gate stale against `f0e77e14`
prompt index, not this change.
