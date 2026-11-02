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
- auto recover invalid transcript (only add, no drop).

### Invarian
- everything in message must store and send.

## Tool
use `which("bash")` as bash.
- bash{command, name = "default", description = "", background = false, detached = false}:
  runs in the persistent shell `name`, or starts a detached bash of that name.
  - key = {session_name, bash_name}.
  - background bash ended will notice its session.
- list_bash{name_regex}: list matched bash with the caller session's name in key.
- check_bash{name = "default"}: return bash's output that hasn't been read by the caller session.
- del_bash{name_regex}: delete matched bash with the caller sessions' name in key. 
- a tool call carries at most one `description`: what the call is for, in the model's words,
  read right after the tool's name.

## Server State
everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.

## Prompt
after the system prompt, before the first user message, inject one message with useful information about session name, harness version (commit id), cwd, time.

### Invariant
transcript that has been sent must not be change.

## Meta
preprocess before saving to transcripts.
- `@image: <path>` , append id to it `@image: <path> (id)`.
  - copy the image to transcript, dedup by path.
- `@skill: <name>` append path metadata to it `@skill: <name> (path)`.
- `@file: <path>` no replace.
in cmd_input, those three should be auto complete.

upload referencing image to provider in advance to get id => OpenAI API File ID mapping. 

replace `id` with mapped File ID before sending for request.

### Meta Prefix Cache Optimize
for the first user message to a empty session, find all meta references recursively with the prompt as the root, find the optimized meta trajectory, append the meta's content with its raw form (without replacement) as index to the system prompt, then the user message with no replacement.

## UI
- main (vertical)
  - horizontal
    - [session{name (click: toggle show, if show change to true, set sticky session, if false and sticky, del sticky session)}] (vertical scroll)
    - [{session(show = true){name, context (in tokens), cached (in n%, latest), tps (latest), ttft (latest), cwd, model, thinking_effort, context_limit}(click: set sticky session), events}] (horizontal split):(vertical)
  - [active_quota{provider, [mouth n%] [week n%] [5h n%] [credit n] [money n¥]}]: update each step end.
  - last cmd | cmd_result (non session related)
  - cmd_input: perfix select, auto complete (match candidates in any char position).
    - `/<cmd>`
    - `<pure text>` expand to `/send_message --message "<pure text>"`: need escape.
  - hot-keys reminder

- event
  - | user_message
  - | assistant_message_item{reasoning | toolcall | output}: streaming.
  - | tool_result: 
    - bash's output streaming.

- events (vertical)
  - [
      |abstract{
        symbol (colored), content (gray),
        |reasoning{beginning (line), ending (line)} (vertical)
        |toolcall{tool_name·description, {name, background, detached, timeout} (bash) | ...} (line)
        |tool_result{ok, time (spent)} (line)
      } (horizontal, 点击展开, block_size, scroll if block_size!=0)
      |output (colored markdown): a table is laid out in the events width, its
       columns shrinking to fit and its cells wrapping inside them
      |(server cmd/cmd_result that related to this session){symbol (colored), content (gray)}
      |error{symbol (colored), content (gray)}
    ] (vertical scroll)
  - streaming: currently stream, block_size, scorll if block_size!=0

- scroll
  - if scrolled from bottom, do not auto scroll, show a go to bottom button.

- hot-key
  - Shift+Enter: new line in cmd_input.
  - PageUp/PageDown (Fn+Up/Down): scroll the sticky session.
  - Shift+Down: scroll the sticky session to bottom.
  - Shift+Delete: delete sticky session.
  - Alt+V: paste (text/image).
  - Shift+Up: last cmd input.


## Cmd
- server:
  - `set_session --session (sticky) <name> --model (sticky) <provider/model> [--thinking_effort <level>](initial: highest in supported or default if unkown) [--context_limit <n%>](initial: "70%") [--cwd <path>](initial: ".")`:
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
  - `set_render [--block_size <n>](initial: 8)`:
    - `block_size`: max line count of non-output block, auto scroll.
  - `set_render_session --session <name> [--show <bool>]`
  - `connect_server [--ip <ip>](default: localhost) --port <port>`
  - `serve --port <port> [--home <path>](default: "~/.gongga")`

## Skill
use python cripts read/write gongga home, calling server cmd.

## Skill: Self Control
do whatever user can do to the server.

## Skill: evolve
restart server should still keep its lifetime, e.g. tui spawned server's lifetime bound to tui.

## Skill: Polity
agents play certain roles to keep context focus.

roles: 
- justice (pro/sota): talk to the user, understand user.
  - judge if plan is conform to the spec and approve to implment. 
  - judge if report is conform to the spec and approve.
  - judge if proposal is in spirit of the spec and send to user for approval.
- premier (flash):
  - orchestrate subagents (flash, pro for hard) to implement the plan.
  - review the implementation
  - present implementation report to justice.
- advisor (sota): plan, proposal
  - present plan for implementation to the justice.
  - present proposal of changes to the spec.
- HR (flash): create justice/premier/advisor session, monitor their situation, fix harness errors (quota, ...).

record:
- proposal{example, expected, actual, actual_spec, expected_spec}
- plan
- report
- judgement over proposal/plan/report{spec violation: none | premise | direct}
- spec amendment{ref proposal, ref judgement}