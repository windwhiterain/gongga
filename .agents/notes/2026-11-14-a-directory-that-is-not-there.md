# A directory this machine does not have

2026-11-14. Session `decode-stream`'s first turn: three `gongga_cli` -> `run_skill` calls,
all refused with

```
could not run "C:\SoftWare\git\Git\usr\bin\bash.exe": 文件名、目录名或卷标语法不正确。 (os error 123)
```

and the model then spent a step concluding the configured bash was broken. It was not: the
path exists, and runs. The session is `remote = lichen-vm` with no `cwd`, so
`Workspace::of` gave it an empty directory and `Workspace::dir(Machine::Local)` answers
empty for a session whose directory is on a peer (`tools/skill.rs` was passing that to
`words::run`, which always calls `Command::current_dir`). Measured directly:

```
subprocess.run([bash, '-c', 'echo hi'])          -> 0, 'hi'
subprocess.run([bash, '-c', 'echo hi'], cwd='')  -> OSError 123 文件名、目录名或卷标语法不正确
```

## The rule (the user's words, and what they map to)

| kind of use | what happens when the directory does not exist |
| --- | --- |
| used directly (a skill's entry, `new_channel`, a file tool) | no error: this server's directory |
| used as a default (`exec`'s `cwd`, which the call may state instead) | refused, with the directory named -- never the program |

## What changed

- `tools::standing_dir`: one answer for every direct use (empty, or not a directory, is
  this server's own). Used by `words::run`, `file::one_shot`, `Channels::start` (a channel
  here; a peer's path is that machine's to answer for).
- `exec`: the resolved `cwd` carries whether it was the session's or the call's, and an
  unusable one is refused with the sentence for that case, before anything starts.
- `relay`: a spawn that still fails says which directory too, so a peer's answer is not
  read as a broken interpreter either.
- `scripts/spawn_dir_selftest.sh`: real server, stand-in model, two sessions (one peer, one
  whose directory was deleted) -- `PASS · spawn-dir`.
