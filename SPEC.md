# Gongga Harness
-- Rust Coding Agent Harness --
## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx". 
- protocol
- server
- cli: cmd fully documented and diagnosed, transparent server cmd wrapper, do not know server cmd.
- tui: transparent cli wrapper.
- ...: more crates may be added, with the same naming and layout.
## Server Client Connection
- UDP.
- key.
## LLM API
standard OpenAI Response API.
- images: File API, gated by the roster's modalities.
## Tool
use `which("bash")` as bash.
- bash{command, name = "default", task = "default", background = false, detached = false}:
  runs in the persistent shell `name`, or starts a detached bash of that name.
  - key = {session_name, bash_name}.
  - `@<path>.<supported image suffix>` will be replaced as file id, referencing image will be upload via File API in advance.
- list_bash{name_regex}: list matched bash with the caller session's name in key.
- check_bash{name = "default"}: return bash's output that hasn't been read by the caller session.
- del_bash{name_regex}: delete matched bash with the caller sessions' name in key. 
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
  - `del_session --session <name>`: also delete all bash with the session name in key.
  - `send_message --session <name> --message <string>`: inject message instantly, turn waiting bash to backgound.
  - `interrupt_session --session <name>`
  - `cancel`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.  
- server_script: that registered by external python scripts
  - `get_messages --session <name> --range <begin:end>`: return the rows in that closed range `range` (negative number to count from end).
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
      - line: fold each non-output message block into one line, streaming keep re-write/override that line.
        - inside single line: re-write without clear text.
        - new line: clear text.
  - `set_event_split --split <bool>`:
    true: split events window by session name, cmd/cmd_result use a separate single line.

## Skill
use python cripts read/write gongga home, calling server cmd.

## Skill: Self Control
do whatever user can do to the server.

## Skill: Polity
agents play certain roles to keep context focus.

consitution: user assign the consitution of a project, usually SPEC file.

roles: 
- justice: talk to the user, understand user, revise consitution according to user's will, judge if developing direction unconsitutional. (only agent writer of consitition)
- pm: breaks the work down, dispatches workers, reviews what comes
  back, runs the tests, gathers the evidence, reports to the justice.

judgement{object, constitution_quote}: should be recorded.
