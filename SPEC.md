# Gongga Harness
-- Rust Coding Agent Harness --
## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx". 
- protocol
- server
- cli: cmd fully documented and diagnosed.
- tui: transparent cli wrapper, do not know cmd.
## Server Client Connection
- UDP.
- key.
## LLM API
standard OpenAI Response API.
- images: File API, gated by the roster's modalities.
## Tool
use GitBash as bash on windows.
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
    - `set_session --session <name> [--cwd <path>] [--llm <provider/model>] [--compact_threshold <n%>] [--thinking-effort <level>]`
    - `del_session --session <name>`
    - `send_message --session <name> --message <string>`: inject message instantly, turn waiting bash to backgound.
    - `interrupt_session --session <name>`
    - `get_messages --session <name> [--after_seq <n>] [--limit <n>]`: read the transcript back,
      oldest first; `--limit` counts back from the end.
    - `cancle`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.
- client:
    - `set_sticky_cmd_argument --name <name> --value <value>`
    - `del_sticky_cmd_argument --name <name>`