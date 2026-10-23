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
- bash{cmd, name = "default", background = false, detached = false}: detached process does not return, so server can cancle while it is running.
    - key = {session_name, bash_name}.
- list_bash: return all bash with the caller session's name key.
- check_bash{name = "default"}: return bash's output that hasn't been read by the caller session.
- del_bash{name}
## Data
- all in database (sqlite), structured for agentic retrieval/mutation.
- database path can be specified at server start. 
## UI
- events[]:
    - cmd
    - cmd_result
    - response{session, reasoning, toolcall, output}: streaming.
    - tool_result{session, toocall, ...}: 
        - bash's output streaming.
- cmd_input: perfix select, auto complete.
    ""
- sticky_cmd_arguments[{name, value}]: auto fill in cmd argument if not provided.
## Cmd
- server:
    - `set_session --session <name> [--cwd <path>] [--llm <provider/model>] [--compact_threshold <n%>] [--thinking-effort <level>]`
    - `del_session --session <name>`
    - `send_message --session <name> --message <string>`
    - `interrupt_session --session <name>`
    - `cancle`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.
- client:
    - `set_sticky_cmd_argument --name <name> --value <value>`
    - `del_sticky_cmd_argument --name <name>`