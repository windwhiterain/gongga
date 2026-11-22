# remote on whiting_main: three independent faults, one of them a false alarm

2026-11-22, prompted by the user asking "remote 功能之前不是修了吗".
The earlier `peer_home` fix (content commit `ff7d6e0`) is NOT regressed. What is
broken is elsewhere, and the `server.usable: null` I reported earlier was misleading.

## Evidence

- `list_remotes --check`: `whiting_main` connected, greeting true, protocol 5,
  `server.usable: null`, reason "the server did not answer".
- `list_paths(remote="whiting_main")` (empty path) -> exit 1,
  `remote Some("whiting_main") did not list ""`.
- `list_paths(remote="whiting_main", path="/home/whting")` -> **exit 0, 22 entries, 27 ms**.
  So the server can reach the peer and list its filesystem.
- `exec(remote="whiting_main", bin="hostname")` fails:
  `the runner this build placed on the peer (/home/whting/.gongga/payload/5/gongga-runner)
  did not answer ... the configured one was refused first:
  /home/whting/.gongga/bin/gongga-d45fe4033c0369c5 runner has no server beside it to relay with`.

## Fault 1 (real, server-side, cross-platform): Windows payload on a Linux peer

`file` on `/home/whting/.gongga/payload/5/*`:
`PE32+ executable (console) x86-64, for MS Windows` for gongga-relay, gongga-runner,
gongga-server. Magic bytes `M Z x \0`. Running one gives a shell syntax error, not a
greeting -- that is why "the runner closed its output".

These three files appeared at 06:34, the same minute I ran `list_remotes --check` and the
remote `exec`. I did not place them by hand. The running server is a Windows build, and the
payload it auto-places on the peer is its own Windows binary set.

By contrast `/home/whting/.gongga/payload/55fd5abc83e5a289/` (03:05, the subagent's install)
is a complete **ELF** set -- client, relay, runner, server, tui -- and its runner answers
`{"event":"hello","protocol":5,"pid":...}` with `gongga-server` sitting beside it.

## Fault 2 (real, configuration): the configured runner cannot relay

`remotes.toml` points at `/home/whting/.gongga/bin/gongga-d45fe4033c0369c5 runner`.
That binary is a good Linux ELF and greets `{"event":"hello","protocol":5}` when run by
hand, but it is the **only** file in `bin/`, and the relay needs a `gongga-server` beside
it -- hence "has no server beside it to relay with", after which the server falls back to
the broken Windows payload of fault 1.

**Fix without any code change**: point `whiting_main.runner` at
`/home/whting/.gongga/payload/55fd5abc83e5a289/gongga-runner runner`.
Linux ELF, protocol 5, server beside it. The server reads the remotes file at startup, so
this needs a config re-read before `exec(remote=...)` can be expected to work.

## Fault 3 (the false alarm, in the shipped remote skill)

`server_verdict` in `ship/skills/remote/_remote.py:603-656` probes with
`list_paths` and **only** the remote name, i.e. an empty directory. Two separate defects
follow:

- `crates/server/src/session.rs:1110-1136` treats `""` as `current_dir()` for a **local**
  endpoint but passes `""` straight through to `space.list(directory)` for a **remote**
  one, which returns None and becomes `did not list ""`. The empty path is only meaningful
  locally, so the skill's probe is refused for a reason that has nothing to do with
  compatibility.
- On that error path the CLI emits no `t == "cmd_result"` JSON frame, and the parser's
  fallback is `usable: None, reason: "the server did not answer"`. The server did answer;
  the reason text is wrong and hides the real message. The parser should treat an error
  reply as an answer carrying its message, and the probe should name a real directory
  (the peer's home, which the skill already resolves).

`usable: null` therefore never was a verdict here -- but "the server did not answer" was
the one branch that told me nothing, and I repeated it instead of reproducing the probe.

## Not done

No code was changed for any of the three faults. Fault 1 is the serious one: any
Windows -> Linux remote hits it. Fault 3 is a small content change. The empty-path
asymmetry in `list_paths` is a third candidate fix, in Rust.
