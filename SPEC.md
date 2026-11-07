# Gongga Harness
-- Rust Coding Agent Harness --

## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx".
- [protocol]: library, define the cmd schema protocol, name some semantic cmd client use directly,
- [server]: binary, define the cmd schema.
- rpc: sending server cmd checked under schema.
  - [cli]: python.
- [client]: libary, receive delta from server, validate at reconnect, update self state and render.
  - [tui]: binary.
- [bundle]: binary, bundle server/cli/tui's binary, route the cmd.
  - `tui [--client_only]`: `bundle server` and connected to it, kill/exit will cancel the server it spawn.
    - `--client_only`: suppress auto spawn server.
  - `server (route to server: [--home <path>])`: extract server/cli into `<home>/bin/`, if previous binary exists and are running and version does not match, cancel server, skill cli, then replace.
- [...]: more crates may be added, with the same naming and layout.

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
result := {id, output, time, mactehd: bool, exit: int|none}
new(name: str, args: list[str] | dict[str], input: optional[str], id: optional[path], wait: optional[time], until: optional[str]) -> result:
  create a new channel.
  - if name is "GONGGA_CLI", route to cmd with session arg sticky to caller.
check(id: path, input: optional[str], wait: optional[time], until: optional[str]) -> result

chennel finished in backgrpund will also notice caller with result.

- wait: if none, wait default time, if 0, return immediately.
- util: use corresponding sticky value according to name, e.g. "python"/"bash".
- id: auto generate if not provided.
- output: new output since last read.
- time: total time cost since the job start.


## Cmd(refactor version)
- rename_job(id: path, new_id: path, session: path) -> result
- ls_job(path: path, session: path) -> list[job]
- del_job(path: path, session: path) -> result


## Server State
everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.

## Prompt
after the system prompt, before the first user message, inject one message with useful information about session name, harness version (commit id), cwd, time. it is stamped once with the session's first prompt row and never re-rendered.

### Invariant
context is append only, context that has been sent must not be change.

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

## Live Compact
- when agent switch from explore to build, set the starting checkpoint of compaction.
- after the starting checkpoint, every time reach a context increase limit, ask the agent to call live compact to compact the history since the last checkpoint.
- live compact summary can reference meta that directly embed in the summary, creating a new checkpoint.

## monitor
- providers: spawn/notice when a session failed on an llm request (refused, or the
  answer never finished). Reasons are the provider's, the roster's or the key's;
  the session is handed the `providers` skill.
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
        |toolcall{tool_name·description, {name, background, timeout} (bash) | ...} (line)
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
  - Alt+Q: exit.

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
  - `cancel --keep_alive <keep_alive>`: interrupt every session then quit, by default kill all channel, optionally make every channel background, keep them alive so retart will adopt them.
  - `trigger_event --event <event>`
  - `subscribe --name <subscription> --pattern <pattern> [--persistant <bool>] [--bash <bash> --cmd <cmd>] [--python <python> --code <code>]`:
    when pattern matched triggered event, run bash cmd or python code in persistant bash or python interpreter.
  - `ls_subscribe --name_regex <subscription>`
  - `del_subscribe --name_regex <subscription>`
  - `skill --name <skill> --action <action>`
    `--action run --entry <entry> <args...>`: run a python function the skill provide as cmd
    `--action ls`: list all skills' metadata.
- server_script: that registered by external python scripts
  - `get_messages [--range <begin:end>] --session <session>(sticky) [--replay <bool>]`: return the items in closed range <begin:end> (negative number count from end, missing number means boundary).
    - `--replay true` answers as a window draws: only the rows a window shows, the
      operator's messages as their own lines, and the parts still streaming under
      the turn they belong to. The result states `last_seq` and `as_of`; `as_of` is
      the stream position the answer is a picture of.
  - `clone_session --session <session> --name <name>`
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
auto choose the latest commit within same protocol, personal(local) change rebase on official(remote).

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
