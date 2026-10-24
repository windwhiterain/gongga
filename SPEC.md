# Gongga Harness
-- Rust Coding Agent Harness --
## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx". 
- protocol
- server
- cli: cmd fully documented and diagnosed.
- tui: transparent cli wrapper, do not know cmd.
- ...: more crates may be added, with the same naming and layout.
## Server Client Connection
- UDP.
- key.
## LLM API
standard OpenAI Response API.
- images: File API, gated by the roster's modalities.
## Tool
use `which("bash")` as bash; no platform special case.
- bash{command, name = "default", task = "default", background = false, detached = false}:
  runs in the persistent shell `name`, or starts a detached bash of that name.
    - key = {session_name, bash_name}.
    - `@<path>.<supported image suffix>` will be replaced as file id, referencing image will be upload via File API in advance.
- list_bash{name_regex}: list matched bash with the caller session's name in key.
- check_bash{name = "default"}: return bash's output that hasn't been read by the caller session.
- del_bash{name_regex}: delete matched bash with the caller sessions' name in key. For a
  persistent shell that discards it (`bash` restarts it); for a detached bash it stops the
  process, which is the only thing that can.
- detach: `bash{detached: true, name = <own name>, command}` starts a bash that is a process of
  its own: no shell owns it, so it outlives the shell, the session, `cancel` and a server
  restart. `name` here is that process's own name, required and unique in the session, and it
  is what `del_bash` matches to end it.
## State
- everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.
- home directory path can be specified at server start (`gongga-server --home <dir>`; this is
  the whole of its state -- database, config, logs, detached-job records, the instance lock).
## UI
- events[]:
    - cmd
    - cmd_result
    - response{session, reasoning, toolcall, output}: streaming.
    - tool_result{session, toocall, ...}: 
        - bash's output streaming.
- cmd_input: perfix select, auto complete.
    - `/<cmd>`
    - `<pure text>` expand to `/send_message --message "<pure text>"`: need escape.
- sticky_cmd_arguments[{name, value}]: auto fill in cmd argument if not provided.
## Cmd
- invocation: `gongga <cmd> [--arg <v> ...]` -- argv is already tokens, so a command line
  carries no `/` and no prose form. `/` belongs to `cmd_input` only.
- server:
    - `set_session --session <name> [--cwd <path>] [--llm <provider/model>] [--compact_threshold <n%>] [--thinking_effort <level>]`
    - `del_session --session <name>`
    - `send_message --session <name> --message <string>`: inject message instantly, turn waiting bash to backgound.
    - `interrupt_session --session <name>`
    - `get_messages --session <name> --seq <a:b>`: the rows in that range, `last_seq`, and the session's `status` (`running`|`idle`). Reads only: creates no session, sets no sticky argument.
    - `cancel`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.
- client:
    - `set_sticky_cmd_argument --name <name> --value <value>`
        - auto set `--session` after `set_session`
    - `del_sticky_cmd_argument --name <name>`
        - auto del `--session` after `del_session`
    - `set_event_filter --name <name> --value<value>`
        - auto set `--session` after `set_session`
    - `del_event_filter --name <name>`
        - auto del `--session` after `del_session`
    - `set_event_detail --mode <mode>`:
        mode:
        - all
        - line: fold each non-output message block into one line, streaming keep re-write that line.
        - one: each session's all message fold into its own block, all cmd cmd_result fold into one block, all block is single line except output message.

## Polity
Roles are split so that whoever writes the law does not write the code, and whoever
writes the code does not judge it. One agent doing both makes "constitutional" a
self-declaration, and the verdict carries no information.
- justice: the session that talks to the user. The only writer of this file, the only
  interpreter of it, the only one who rules. It rules high and touches nothing: no
  implementation, no dispatch to workers, no test runs, no evidence gathering, no reading
  code to make up its mind. It commissions the pm, and reads what comes back.
- pm: a separate session. Breaks the work down, dispatches workers, reviews what comes
  back, runs the tests, gathers the evidence, reports to the justice. It cannot amend
  this file, and cannot rule on its own work.
- worker: does the one task it was given, and returns evidence for it.
- a ruling cites the clause it rests on, by section and line. No clause, no ruling.
- a ruling is one of: constitutional, unconstitutional, constitutional with conditions.
- the justice rules on a report but never takes one on faith: it names the evidence it
  wants -- a command and its output, a path, a diff -- and rules on whether that evidence
  answers the question. A report that says "done, tests pass" without it is sent back,
  not ruled on. Demanding evidence is not gathering it.
- the justice never polls for a report. It dispatches, ends its turn, and is woken by the
  pm's message. A wait loop in the justice's session is a design error, not a technique:
  the turn boundary already is the wait, and polling for it burns processes and per-session
  state to learn something the wake would have told it.
- so the wake must be wired: the court records which session the justice is, and waking it
  takes no name the pm has to guess. A command taught in a charter that cannot run as
  written is a bug in the instrument, not a mistake by whoever copied it.
- the pm may wait on its own workers, since that is work it owns; what it waits with must
  not write the user's per-session client state.
- a clause no ruling can rest on -- "keep it clean", "be careful" -- is not a clause.
