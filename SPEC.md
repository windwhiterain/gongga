# Gongga Harness
-- Rust Coding Agent Harness --

## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx". 
- protocol
- server: binary (the backend program; also a library for its own tests).
- cli: binary.
  - cmd fully documented and diagnosed, transparent server cmd wrapper, do not know server cmd.
  - does not carry the server: `serve` starts the server program.
- tui: binary, transparent cli wrapper, auto spawn a server and connected to it.
  - `--client_only`: suppress auto spawn server.
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
- `messages` is the transcript: one row per entry, ordered by `seq`, each row written as it happens --
  a tool result lands when that tool finished, not when its step did.
  - `content`: the entry as the LLM API takes it, one standard OpenAI Response API input item, sent
    verbatim; `NULL` means no request is built from that row (a notice, the prompt in force).
  - `display`: what only the reader needs -- a reasoning summary, an image reference, a notice's text, the
    prompt in force -- and is never sent.
  - `call_id`: the call a call or result row names, so a request pairs them by name rather than by
    opening the item's bytes.
- a request's `input` is the `content` of the non-`NULL` rows in `seq` order, with the handover note first.
  Two transforms are allowed, and no others:
  - a reference becomes its content: `display`'s image becomes an `input_image` part -- the provider's file
    id once the File API has minted one, otherwise the bytes read as the request is written -- and a
    provider tool item's payload file is read the same way. Either way a transcript replayed ten times does
    not carry ten copies of a screenshot, and an item whose file is gone leaves the request out.
  - a step: each `function_call` is followed directly by its own `function_call_output`. A step is over when
    its last open call is answered, or when a new user prompt ends a turn that never finished -- that step's
    unanswered calls are then sent as a placeholder saying they were interrupted. Rows a step wrote between
    its calls (its own reasoning) stay inside the step. A result whose call is not in the same step is left
    out: a call and its result belong to one step, which is what makes that decidable without remembering
    what was already sent.
- the request body streams. Repairing a step holds that step's rows and nothing older, so what a request
  costs in memory is O(one step), never O(history).
- `upto_seq` is pinned when a request starts, and every retry of that request replays the same window.
- migration: the home's version step renames the payload version 1 stored to `display` and opens an empty
  `content`; the server fills `content` as it opens, because the rule that says what a row sends lives
  beside the code that writes rows. A transcript that stored its results in completion order is not
  rewritten -- the step repair handles it on the way out, like any other.
## UI
- main (vertical)
  - horizontal
    - sessions[] (vertical scroll)
      - click to set sticky session.
    - [{session{render mode = full}, events (scroll)}] (horizontal split):
      - click to set sticky session.
  - [{session{render mode = line}, event}] (vertical lines scroll)
    - click to set sticky session.
  - [{cmd, cmd_result}] (line)
  - stickys[{name, value}]: auto fill in cmd argument if required but not provided.
  - cmd_input: perfix select, auto complete (match candidates in any char position).
    - `/<cmd>`
    - `<pure text>` expand to `/send_message --message "<pure text>"`: need escape.

- event
  - | message_item{session | reasoning | toolcall | output}: streaming.
  - | tool_result{session, toocall, ...}: 
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
  - `set_session --session <name> [--cwd <path>] [--llm <provider/model>] [--context_limit <n%>] [--thinking_effort <level>]`:
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
