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
## Tool
use GitBash as bash on windows.
- bash{command, name = "default", task = "default", background = false, detached = false}: detached process does not return, so server can cancle while it is running.
    - key = {session_name, bash_name}.
- list_bash: return all bash with the caller session's name in key.
- check_bash{name = "default"}: return bash's output that hasn't been read by the caller session.
- del_bash{name_regex}: only mactch bash with the caller sessions' name in key.
- kill_bash{name = "default", job = <n>}: stop detached jobs -- one job, or every job of one
  shell. A detached job is not any shell's, so it outlives `del_bash` and the server; this is
  the only thing that ends it.
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
    - `send_message --session <name> --message <string>`
    - `interrupt_session --session <name>`
    - `get_messages --session <name> [--after_seq <n>] [--limit <n>]`: read the transcript back,
      oldest first; `--limit` counts back from the end.
    - `cancle`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.
- client:
    - `set_sticky_cmd_argument --name <name> --value <value>`
    - `del_sticky_cmd_argument --name <name>`