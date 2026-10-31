# Gongga Harness
-- Rust Coding Agent Harness --

## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx". 
- protocol
- server: binary (the backend program; also a library for its own tests).
- cli: binary.
  - cmd fully documented and diagnosed, transparent server cmd wrapper, do not know server cmd.
- tui: binary, transparent cli wrapper.
- bundle: binary, bundle server/cli/tui's binary, route the cmd.
  - `tui [--client_only]`: `bundle server` and connected to it, kill/exit will cancle the server it spawn.
    - `--client_only`: suppress auto spawn server.
  - `server (route to server: [--home <path>])`: extract server/cli into `<home>/bin/`, if previous binary exists and are running and version does not match, cancle server, skill cli, then replace.
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
- list_bash{name_regex}: list matched bash with the caller session's name in key.
- check_bash{name = "default"}: return bash's output that hasn't been read by the caller session.
- del_bash{name_regex}: delete matched bash with the caller sessions' name in key. 

## State
everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.

## File
`@<path>.<supported image suffix>` in messages will be replaced as file id, referencing image will be upload via File API in advance.

## UI
- main (vertical)
  - horizontal
    - [session (render mode = none){name (double click change mode to full)}] (vertical scroll)
    - [{session (render mode = full){name (double click change mode to line), context (in tokens), cached (in n%, latest), tps (latest), ttft (latest), cwd, provider, thinking_effort, context_limit}, events (vertical scroll)}] (horizontal split):
  - [{session (render mode = line){name (double click change mode to none)}, event} (line)] (vertical)
  - [active_quota{provider, [mouth n%] [week n%] [5h n%] [credit n] [money n¥]}]: update each step end.
  - [{cmd, cmd_result}] (line)
  - cmd_input: perfix select, auto complete (match candidates in any char position).
    - `/<cmd>`
    - `<pure text>` expand to `/send_message --message "<pure text>"`: need escape.

- session{name}: click to set session sticky.

- event
  - | user_message
  - | assistant_message_item{reasoning | toolcall | output}: streaming.
  - | tool_result: 
    - bash's output streaming.

- scroll
  - if scrolled from bottom, do not auto scroll, show a go to bottom button.

- hot-key
  - Shift+Enter: new line in cmd_input.
  - PageUp/PageDown (Fn+Up/Down): scroll the sticky session.
  - Shift+Down: scroll the sticky session to bottom.


## Cmd
- invocation: `gongga-cli <cmd> [--arg <v> ...]` -- argv is already tokens, so a command line
  carries no `/` and no prose form. `/` belongs to `cmd_input` only.
- server:
  - `set_session --session <name> [--cwd <path>] [--llm <provider/model>] [--thinking_effort <level>] [--context_limit <n%>]`:
    - plus `set_sticky_cmd_argument --name session --value <session>`
  - `del_session --session <name>`: also delete all bash with the session name in key.
  - `send_message --session <name> --message <string>`: inject message instantly, turn waiting bash to backgound.
  - `interrupt_session --session <name>`
  - `cancel`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.  
- server_script: that registered by external python scripts
  - `get_messages --session <name> --range <begin:end>`: return the rows in that closed range `range` (negative number to count from end).
- client:
  - `set_sticky --name <name> --value <value>`:
  - `del_sticky --name <name>`
  - `set_render [--block_size <line>]`:
    - `block_size`: max line count of non-output block, auto scroll.
  - `set_render_session --session <name> [--mode <none|line|full>]`
  - `connect_server [--ip <ip>](default: localhost) --port <port>`
  - `serve --port <port> [--home <path>](default: "~/.gongga")`
  - `skill --session <name> --skill <name>`: send skill metadata to agents.

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
