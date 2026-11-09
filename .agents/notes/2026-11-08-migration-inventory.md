# Migration inventory: what still holds the pre-channel tool layer

Build `6adff43`, read 2026-11-08. Read-only recon for deleting
`tools/{bash,exec,exec_remote,python,interpreter,python_remote,detached}.rs` and
`remote/{shell,shell_set}.rs` (5201 lines by `wc -l`) together with the `[tool.*]` rows
they serve. The plan itself is `docs/notes/server-channel.md`, "Still owed: the eighteen
go"; this file is the inventory that plan is missing.

## 0. Four things the line count hides

1. **The tool facades are not in the nine files.** `BashTool`, `CheckBashTool`,
   `CheckJobTool`, `DelBashTool`, `ListBashTool`, `DelJobTool` and `WaitTool` all live in
   `tools/mod.rs:1559-2488` (about 800 lines), not in `tools/bash.rs`. `tools/bash.rs`
   is the shell process itself. So deleting the file leaves the tool registrations
   behind, and deleting the registrations leaves `WaitTool` (which stays) in the middle
   of the block.
2. **`tools/python.rs` also owns the three tools that must stay.** `ReadFileTool`
   (:857), `WriteFileTool` (:942) and `ReplaceTool` (:1016) are in it. They are the
   model's only door to a file and the plan keeps them, so `python.rs` has to be split
   before it can be deleted -- the REPL half goes, the file half moves.
3. **`tools/detached.rs` is not only a tool.** Nine surviving modules call its free
   functions (`kill_tree`, `signal_group`, `reap_child`, `alive_pids`, `release_ended`,
   `arm_adopted`, `names_process`, `stop_process`, `release_detached`,
   `DetachedJob::launch_program`, `DetachedRegistry`): `channel.rs`, `tunnel.rs`,
   `words.rs`, `worker.rs`, `script.rs`, `commands.rs`, `agent.rs`, `lib.rs`,
   `remote/runner.rs`. It is the process reaper as much as it is the `background`
   carrier; slice 4 is a rewrite, not a delete.
4. **`session.rs` implements a trait declared in `tools/mod.rs`.** The `Shells` trait and
   its `on_endpoint` (mod.rs:190-214) are implemented at `session.rs:80`;
   `ToolContext::cap` (mod.rs:235, uses `on_endpoint` at :246) is called by survivors
   (`skill.rs:109` still does). The shell cluster cannot go without deciding what
   replaces `on_endpoint`.

## 1. Who references each old file

Verdict: **delete** = the line goes with the file; **change** = a surviving file's line
that must be rewritten or moved.

### `crates/server/src/tools/bash.rs` (752 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `tools/mod.rs` | 24 `pub mod bash;` | delete |
| `tools/mod.rs` | 21 `use crate::tools::bash::{BashChunk, BashStream};` | delete |
| `tools/mod.rs` | 1850 `e.is::<crate::tools::bash::ShellEnded>()` | change -- `BashTool`'s stream-failure path |
| `remote/shell.rs` | 15, 426, 708, 716, 731, 736, 743, 747, 757 | delete (both files go) |
| `remote/shell_set.rs` | 16, 154 | delete (both files go) |
| `session.rs` | 1332 `-> anyhow::Result<crate::tools::bash::BashOutcome>` (`Shells::run`) | change -- the last surviving caller of the old shell |
| `tools/interpreter.rs` | 12, 120, 124 | delete |
| `docs/notes/server-tools-bash.md` | 1, 16, 256 | delete the file |
| `docs/notes/README.md` | 130 (index row) | delete the row |
| `docs/notes/server-channel.md` | 152, 165 | change -- it is the plan; rewrite when the slice lands |
| `docs/notes/skill-lib.md` | 79 (`msys_path` "in `tools/bash.rs`") | delete -- `msys_path` exists nowhere in `crates/`; the sentence is already stale |
| `docs/notes/server-process.md` | 5 | change |

### `crates/server/src/tools/exec.rs` (229 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `tools/mod.rs` | 27 `pub mod exec;` | delete |
| `tools/mod.rs` | 772 `exec::ExecTool { .. }` | delete the arm |
| `docs/notes/server-tools-exec.md` | 1 | delete the file (it is not in the README index any more) |

### `crates/server/src/tools/exec_remote.rs` (91 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `tools/mod.rs` | 28 `pub mod exec_remote;` | delete |
| `tools/exec.rs` | 18 `use crate::tools::exec_remote;` | delete (both go) |

### `crates/server/src/tools/python.rs` (1100 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `tools/mod.rs` | 34 `pub mod python;` | delete |
| `tools/mod.rs` | 785-855 the python arm (`PythonTool`, `CheckPythonTool`, `DelPythonTool`, `ListPythonTool`, `ReadFileTool`, `WriteFileTool`, `ReplaceTool`) | change -- split it: the first four arms go, the last three stay |
| `tools/interpreter.rs` | 13 | delete |
| `tools/python_remote.rs` | 12 | delete |
| `tools/skill.rs` | 159 `.or_else(|| crate::tools::python::detect().ok())` | change -- `crate::config::detect_python` (`config.rs:308`) already does this job |
| `docs/notes/server-tools-python.md` | 1 | delete the file |

### `crates/server/src/tools/interpreter.rs` (165 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `session.rs` | 486, 632 (`PythonMap<String, Arc<Interpreter>>` on the session's tool context) | change -- the per-session interpreter map goes with it |
| `tools/python.rs` | 190, 191, 328, 529, 536, 556, 672, 711 | delete |
| `tools/python_remote.rs` | 128, 129 | delete |

### `crates/server/src/tools/python_remote.rs` (231 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `tools/mod.rs` | 35 `pub mod python_remote;` | delete |
| `tools/python.rs` | 276, 390, 844 | delete |

### `crates/server/src/tools/detached.rs` (1383 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `tools/mod.rs` | 26 `pub mod detached;`, 22 `use ..DetachedRegistry` | delete |
| `tools/mod.rs` | 2279 `stop_process(pid)` (in `DelJobTool`) | delete with the tool |
| `tools/channel.rs` | 261 `DetachedJob::launch_program` | change -- a channel is what outlives a turn now; this is the one caller with a home |
| `tools/tunnel.rs` | 224, 285, 297, 386, 480, 582, 588, 589, 697 | change |
| `tools/words.rs` | 102 `DetachedJob::launch_program` | change |
| `worker.rs` | 163 `DetachedRegistry::new(..)` | change |
| `agent.rs` | 24 | change |
| `lib.rs` | 67 | change |
| `session.rs` | 17 | change |
| `commands.rs` | 2578 `stop_process(pid)`, 2729 `release_detached()` | change |
| `script.rs` | 194 `disarm_detached`, 195 `kill_tree` | change |
| `remote/runner.rs` | 619 `signal_group`, 628 `kill_tree` | change |
| `remote/shell_set.rs` | 394 `kill_tree` | delete |
| `tools/bash.rs` | 543, 549, 552 | delete |
| `docs/notes/server-tools-detached.md` | 1 | delete the file |
| `docs/notes/README.md` | 131 | delete the row |
| `docs/notes/server-process.md` | 5 | change |
| `docs/notes/server-tools-bash.md` | 16 | delete (the file goes) |

### `crates/server/src/remote/shell.rs` (805 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `remote/mod.rs` | 10 `pub mod shell;` | delete |
| `remote/shell_set.rs` | 14 `use crate::remote::shell::{Door, RemoteShell};` | delete (both go) |
| `docs/notes/server-remote.md` | 257 (the "`remote::shell` and `remote::shell_set`" section) | change |
| `docs/notes/server-channel.md` | 153, 169 | change |

### `crates/server/src/remote/shell_set.rs` (445 lines)

| referrer | line | verdict |
| --- | --- | --- |
| `remote/mod.rs` | 11 `pub mod shell_set;`, 16 `pub use shell_set::{Shell, ShellSet};` | delete |
| `session.rs` | 15, 63, 77, 89, 104, 174, 204, 216, 243, 262, 277, 292, 323, 326, 348, 1195, 1342 | change -- `session.rs` is where `Shells` is implemented (:80) |
| `tools/mod.rs` | 17 `use ..shell_set::Shell`, 198 `Discarded`, 190-214 (`Shells` trait), 246 (`on_endpoint` inside `cap`) | change |
| `docs/notes/server-remote.md` | 257 | change |

## 2. The `[tool.*]` rows

All paths below are `ship/config/config.toml`. Every row in the first table is already
`offered = false`: the command layer reaches it by name (`gongga-cli bash ...`), a
subscription action may name it, and the model never sees it.

| row (line) | `cmd` | what its `summary` promised | fate |
| --- | --- | --- | --- |
| `[tool.bash]` 645 | `bash` | a persistent Git Bash per `name`, carrying cwd/exports/functions; output streamed; long output capped to a file; `background` starts a process of its own | delete the row -- `new bash` is that shell (`[channel.bash]` 540 already holds its spawn/write/until) |
| `[tool.remote_bash]` 665 | `bash`, `machine = "remote"` | the same on the session's peer | delete -- `new bash --remote <peer>` |
| `[tool.exec]` 692 | `exec` | one program with its arguments, no shell, nothing to escape, no cwd/export survival | delete -- `new <program> --args '[...]'` |
| `[tool.remote_exec]` 708 | `exec`, `machine = "remote"` | the same on the peer's PATH | delete -- `new <program> --remote` |
| `[tool.check_bash]` 724 | `check_bash` | unread shell output, reading clears it, empty means nothing yet | delete -- `check <id>` |
| `[tool.check_job]` 740 | `check_job` | what became of a `background` call: `running`/`ok`/`error`/`lost` | delete -- `check <id>`; a channel's status is its row |
| `[tool.del_bash]` 773 | `del_bash` | discard shells whose name matches a regex, jobs die with them | delete -- `del_channel <path>` |
| `[tool.del_job]` 785 | `del_job` | stop a job by id, forget a finished one | delete -- `del_channel <path>` |
| `[tool.list_bash]` 797 | `list_bash` | names, cwds, unread output, running jobs | delete -- `ls_channel <path>` |
| `[tool.python]` 809 | `python` | `bash` with Python instead of a shell, `restart` to drop the namespace | delete -- `new python` (`[channel.python]` 548) |
| `[tool.remote_python]` 833 | `python`, `machine = "remote"` | the same on the peer | delete -- `new python --remote` |
| `[tool.check_python]` 858 | `check_python` | `check_bash`, for an interpreter | delete -- `check` |
| `[tool.del_python]` 874 | `del_python` | `del_bash`, for an interpreter | delete -- `del_channel` |
| `[tool.list_python]` 886 | `list_python` | `list_bash`, for an interpreter | delete -- `ls_channel` |

Rows that stay, with the row's line:

| row | `cmd` | note |
| --- | --- | --- |
| `[tool.new]` 561, `[tool.check]` 612 | `new`, `check` | the surviving surface |
| `[tool.wait]` 758 | `wait` | SPEC keeps it; `owns_its_wait = true` |
| `[tool.read_file]` 898, `[tool.write_file]` 926, `[tool.replace]` 950 | same names | stay -- but their structs are in `tools/python.rs` (section 1), so the row survives the file only if the structs move first |
| `[tool.skill]` 984, `[tool.live_compact]` 1009 | same | stay |

**No row becomes a `[tool.X] cmd = "<cmd>"` forward.** The fallback already exists --
`tools/mod.rs:856 other => Box::new(Forward { name })` is SPEC's `fn <tool>(**args)` -- so
a row that named a server command would work with no code at all. It is simply wrong for
these fourteen: `new` does not take `command`, and `ls_channel` does not take
`name_regex`, so repointing `[tool.list_bash] cmd = "ls_channel"` would forward arguments
the command never declared. Delete the rows; the model spells `new` / `check`.

The one caller that is not the model is a subscription action naming a tool command
(`tools/mod.rs:2045` area's comment in the config says so explicitly). Those have to be
re-pointed at `new_channel` / `input_channel` before the rows go, or a stored
subscription dies quietly on the next event.

### Prose that goes with them (same file, not `[tool.*]`)

| block | lines | verdict |
| --- | --- | --- |
| `[tools.arguments]` `background`, `timeout_ms` | 529, 531 | change -- both name `check_job`, `check_bash`, `check_python`, `remote_bash`; live (`config.rs:417` feeds `general_parameters`) |
| `[tool_text.bash]`, `.remote_bash`, `.exec`, `.remote_exec`, `.check_bash`, `.check_job`, `.del_bash`, `.del_job`, `.list_bash`, `.python`, `.remote_python`, `.check_python`, `.del_python`, `.list_python` | 352-473 (fourteen blocks) | delete -- dead config: `config.rs:358` says "Where the rows used to live. Read for nothing" |
| `[tool_text.general]` 347, `.wait` 403, `.read_file` 481, `.write_file` 491, `.replace` 500, `.skill` 511, `.live_compact` 474 | as listed | keep -- except `general.arguments.background`/`timeout_ms`, which name `check_job`; change those two |
| `[prompts] dynamic_tail_remote` (194-195), `dynamic_tail_remote_unstated` (201-202) | 195, 202 | change -- live prompt text: names `remote_bash`, `bash`, `python`, `read_file`, `write_file`, `replace` as the way to work on a peer |

## 3. The tests

| script | lines that use an old tool | fate |
| --- | --- | --- |
| `scripts/job_selftest.sh` | 65 (the `run()` helper), 75/109/128/149/176/182/188 (`exec`), 88/92/98/102/120/137/229/245 (`check_job`), 133/151 (`del_job`), 161 (`bash`), 164 (`check_bash`), 212 (a hand-seeded `exec` JobRow) | **retire.** Its whole subject is the background-job carrier (`detached.rs`): `job_id`, `--detached`, the `lost` status, adoption after a restart. A channel is that carrier now; the coverage belongs in `channel_selftest.sh`, not in a file about a tool that no longer exists |
| `scripts/remote_selftest.sh` | 212/414 (`remote_bash`), 358 (`bash`), 362 (`list_bash`), 381/386 (the `"bash"` key of the list payload), 404 (`{"bash","remote_bash"} <= names`), 266/278/288/299/315/326 (`write_file`/`read_file`/`replace`) | **rewrite, split in two.** The file-tool half (266-336) stays verbatim -- those three tools stay. The shell half moves onto `new --remote` / `check`. Line 404 flips to asserting the offered set has no `bash` |
| `scripts/cmd_selftest.sh` | 125-128, 132 (`bash main <command>`), 136-139 (`python main <code>`), 141-144 (`read_file main <path>`) | **rewrite.** Sections 6 and 8 prove a tool row is reachable as a command; re-point them at `new_channel` / `input_channel`, which is the command-layer door that stays. Section 9 (`read_file`) is untouched |
| `scripts/tool_selftest.sh` | 188-192 (the offered set: `new`/`check`/`wait` in, `bash`/`read_file` out), 197 (`new '{"name":"bash"}'` -- the *new* tool, keep as is) | **keep.** Line 191-192 is the assertion this migration is measured by; when the rows go it should widen from "not offered" to "not registered at all" |
| `scripts/disk_selftest.sh` | 179, 201, 202 (`bash spill "echo hello"`), 193-194 (`payload["tool"] == "bash"`) | **rewrite.** A working call now needs a channel (`new bash ...` plus an input), and the `disk_space` payload's `tool` field becomes whatever that call is named |
| `scripts/channel_selftest.sh` | 145, 189, 200, 204 (`new main bash ...`) | **untouched** -- already the channel world; `bash` here is a program name, not a tool |
| `scripts/channel_remote_selftest.sh` | none | untouched |
| `scripts/{append_only,bundle,client_state,draft,drain,evolve,heir,install,meta,monitor,prompt,selfcontrol,ship,subagent,subscription,upload,validity,watchdog}_selftest.sh` | none -- no `bash`/`exec`/`python`/`check_*`/`del_*`/`list_*`/`remote_*` invocation, and no quoted tool name | untouched |

The three scripts the plan names as the measuring stick are `channel_selftest.sh`,
`channel_remote_selftest.sh` and `tool_selftest.sh`; of those only `tool_selftest.sh` has
a line to change. What must move is `job_selftest.sh` (retire), `remote_selftest.sh`
(split), `disk_selftest.sh` (re-point) and the two sections of `cmd_selftest.sh`.
