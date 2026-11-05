# Gongga Harness
-- Rust Coding Agent Harness --

## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx".
- protocol
- server: binary (the backend program; also a library for its own tests).
- client: use protocal to send server cmd
  - cli: python.
    - cmd fully documented and diagnosed.
  - tui: binary.
- bundle: binary, bundle server/cli/tui's binary, route the cmd.
  - `tui [--client_only]`: `bundle server` and connected to it, kill/exit will cancel the server it spawn.
    - `--client_only`: suppress auto spawn server.
  - `server (route to server: [--home <path>])`: extract server/cli into `<home>/bin/`, if previous binary exists and are running and version does not match, cancel server, skill cli, then replace.
- ...: more crates may be added, with the same naming and layout.

## Server Client Connection
- UDP.
- key.
- the server sends deltas only: a session change is that one row, a message is
  the part that grew, and nothing re-sends a table a client already has. The whole
  session list travels once, in the greeting, because that is the one moment a
  client has none.
- every event on the stream carries its position (one counter, one step per
  published event), so "what did this client miss" is a comparison and never a
  guess. A command's own answer has no position: it belongs to its connection.

## LLM API
standard OpenAI Response API/Anthropic API.
- images: File API, gated by - auto recover invalid transcript (only append, no drop).

### Invarian
- everything must store and send.

## Session name
path, rendered in hierarchy.
## Tool
tool are tunnel of cmd that agents can directly call to avoid escape.

all tool has optional `description`, `max_output`, `timeout_ms`, `background`, `detached` argument that the tunnel consume.
- `max_output`: in chars, if exeeded, save to temp file and return the file metadata {path, chars, line count}
- foreground job running over timeout become background.
- job running over certain time auto become detached. long job is forced to use detached + wait that job and wait can live across cancel.
- detached job lives across server restart, adopted when server restart.
- background/detached job ended will notice caller session, missing detacthed job, likely finished or killed when the server is down, will also notice.

wait is the only expecial tool that treated internally.

use `which("bash")` as bash.
- bash{command, name = "default", detached = false}:
  cwd is session's cwd if session has no remote.
  runs in the persistent shell `name`
  - key = {session_name, bash_name}.
- exec{command}: so shell to avoid escape.
- python{code, ...}: mirror bash, persistant REPL.
- list_bash{name_regex}: list matched bash with the caller session's name in key.
- list_python{...}: mirror list_bash.
- check_bash{name = "default", wait_ms = 0}: return bash's output that hasn't been read by the caller session. With `wait_ms > 0` and background jobs still running, wait until they finish or the wait expires, then return the unread output: a timeout leaves the jobs running.
- check_python{...}: mirror check_bash.
- del_bash{name_regex}: delete matched bash with the caller sessions' name in key.
- del_python{...}: mirror del_bash.
- remote_bash{command, name, target = <session's remote>}:
  cwd is session's cwd if session's remote is target.
  non-meta reference the remote skill.
- remote_exec{command}: mirror exec.
- remote_python{code, ...}: mirror remote_bash.
- read_file/write_file/replace{remote = none}:
- wait{}: if interrupted, the call end.

## Server State
everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.

## Prompt
after the system prompt, before the first user message, inject one message with useful information about session name, harness version (commit id), cwd, time. it is stamped once with the session's first prompt row and never re-rendered.

### Invariant
transcript is append only, transcript that has been sent must not be change.
## Meta
preprocess before saving to transcripts.
- `@image(<path>)` , append id to it `@image(<path>) (id)`.
  - copy the image to transcript, dedup by path.
- `@skill(<name>)` append path metadata to it `@skill(<name>) (path)`.
- `@file(<path>)` no replace.
in cmd_input, those three should be auto complete.

upload referencing image to provider in advance to get id => OpenAI API File ID mapping.

replace `id` with mapped File ID before sending for request.

### Meta Prefix Cache Planning
a cache tree is simulated for each model or (model, some config) that shares prefix cache.

for the first user message to a empty session, find all meta references recursively with the prompt as the root, plan the price optimal meta content order in the cache tree, append the meta's content with its raw form (without replacement) as index to the system prompt, then per session dynamic content.

## monitor
- session: spawn/notice when any session is error.
- disk: spawn/notice when free space is under certain threshold.

## UI
- main (vertical)
  - horizontal
    - [session{name (click: toggle show, if show change to true, set sticky session, if false and sticky, del sticky session)}] (vertical scroll, tree: a prefix of a name is a group row, a click on its marker folds it away)
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
      } (horizontal, 点击展开, block_size, box_edge, scroll 不溢出到外层)
      |output (colored markdown): a table is laid out in the events width, its
       columns shrinking to fit and its cells wrapping inside them
      |(server cmd/cmd_result that related to this session){symbol (colored), content (gray)}
      |error{symbol (colored), content (gray)}
    ] (vertical scroll)
  - streaming: currently stream, block_size, scorll if block_size!=0

- scroll
  - if scrolled from bottom, do not auto scroll, show a go to bottom button.
  - an opened block never hands its scroll to the events: its rules and its `box_edge`
    columns are the events'. scrolling where the pointer is decides, once.

- hot-key
  - Shift+Enter: new line in cmd_input.
  - PageUp/PageDown (Fn+Up/Down): scroll the sticky session.
  - Alt+Down: scroll the sticky session to bottom.
  - Shift+Delete: delete sticky session.
  - Alt+V: paste (text/image).
  - Alt+C: copy the selection.
  - Shift+Up/Down: last/next cmd input.

## Sticky
stikcy argument will auto fill:
- non optional corresponding argument in cmd if missing.
- optional corresponding argument's initial value (if it has).
stikcy to xxx means del its sticky if sticky xxx is changed


## Cmd
all named argument's name can be ommited via argument order.

- server:
  - `set_session --name <session>(sticky) [--model <model>](initial: none)(sticky) [--thinking <thinking>](initial: default)(stikcy to model) [--ctx_limit <ctx_limit%>](initial: "70%")(tsikcy to model) [--cache_iter <cache_iter>](initial: shortest)(sticky over model) [--remote <remote>](initial: none) [--cwd <cwd>](initial: ".")(sticky to remote) [--must_create <bool>](initial: false)`:
    - `--must_create true` refuses the command when the session already exists: a caller
      that starts its own session must not join another one's transcript by reusing a name.
  - `del_session --name <session>(sticky)`: also delete all bash with the session name in key.
  - `send_message --message <message> --name <session>(sticky)`: inject message instantly, turn waiting bash to backgound.
  - `interrupt_session --name <session>(sticky)`
  - `cancel`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.
  - `trigger_event --event <event>`
  - `subscribe --subscription <subscription> --pattern <pattern> [--persistant <bool>] [--bash <bash> --cmd <cmd>] [--python <python> --code <code>]`:
    when pattern matched triggered event, run bash cmd or python code in persistant bash or python interpreter.
  - `del_subscribe --subscription <subscription>`
  - `skill --name <name> [--args <word list as JSON>]`: run the entry a skill declares,
    resolved by the roots below, with the words as given and no shell between them. The
    program that starts it is read off the file (`.sh` is bash, `.py` is python, `.ps1` is
    powershell, `.cmd` is `cmd /c`, and no extension means its `#!` line), so the caller
    names the script's arguments and not its interpreter. The entry's exit code is this
    command's `ok`, and what it printed is what comes back. A skill with no entry is a body
    and refuses, naming the `SKILL.md` to read instead. Published by the tool of the same
    name, which is the one registration both surfaces come from.
- server_script: that registered by external python scripts
  - `get_messages [--range <begin:end>] --session <session>(sticky) [--replay <bool>]`: return the items in closed range <begin:end> (negative number count from end, missing number means boundary).
    - `--replay true` answers as a window draws: only the rows a window shows, the
      operator's messages as their own lines, and the parts still streaming under
      the turn they belong to. The result states `last_seq` and `as_of`; `as_of` is
      the stream position the answer is a picture of.
  - `clone_session --session <session> --name <name>`
- command layer, without a server:
  - `skill ls` | `skill run <name>` | `skill <name> [args...]`: the same entry, run on
    this disk and not on a server -- why a machine with no server still runs a skill.
    `argv` after the name is the entry's, unparsed, so a flag spelled there is the script's
    and not this layer's; the entry's exit code is this process's. `run` reaches a skill
    whose own name is `ls` or `run`.
    - what leaves this layer: `ls` (its answer is the session's, so it is asked of the
      server when the shell names one) and `--name`/`--args`, which spell the server's own
      form, `skill <session> --name <n> --args <json>`. A skill that must receive `--name`
      itself is `skill run <name> --name …`.
    - what a skill runs is its `SKILL.md`'s `metadata.entry`, and `skill ls` gives that
      `SKILL.md`; there is no verb for "which file", since reading the body says it.
- client:
  - `set_sticky --type <type> --value <value>`:
  - `del_sticky --type <type>`
  - `set_render [--block_size <n>](initial: 8) [--box_edge <n>](initial: 2)`:
    - `block_size`: max line count of non-output block, auto scroll.
    - `box_edge`: columns at each side of an opened block where the wheel scrolls the
      events instead of the block; the block is drawn there like anywhere else.
  - `set_render_session --show <bool> --session <session>(stikcy)`
  - `connect_server [--ip <ip>](default: localhost) --port <port>`
  - `serve --port <port> [--home <home>](default: "~/.gongga")`

## Git Managed Config
personal(local) change always rebase on official(remote).

## Skill
provider uv python package, API can be called by harness cmd or python import.

## Skill: Self Control
do whatever user can do to the server.

## Skill: subagent
- create a new session under caller session's name.
- subscribe new session's turn end event.
- send_message to new session.
- optionaly wait.


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

record:
- proposal{example, expected, actual, actual_spec, expected_spec}
- plan
- report
- judgement over proposal/plan/report{spec violation: none | premise | direct}
- spec amendment{ref proposal, ref judgement}
