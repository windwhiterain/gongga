# Gongga Harness
-- Rust Coding Agent Harness --

## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx".
- [protocol]: library, define the cmd schema protocol, name some semantic cmd client use directly,
- [server]: binary, define the cmd schema.
- rpc: sending server cmd checked under schema.
  - [cli]: python.
- [client]: binary.
  - [tui]: binary.
- [bundle]: binary, bundle server/cli/tui's binary, route the cmd.
  - `tui [--client_only]`: `bundle server` and connected to it, kill/exit will cancel the server it spawn.
    - `--client_only`: suppress auto spawn server.
  - `server (route to server: [--home <path>])`: extract server/cli into `<home>/bin/`, if previous binary exists and are running and version does not match, cancel server, skill cli, then replace.
- [...]: more crates may be added, with the same naming and layout.

## Client
save a copy of the server states.
- server send delta, client update.
- validate against server, delete invalid states.
- request only what is missing and need rendering.
- granularity must be the finest.

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
- images: File API.
- auto validate and fix the latest saved step that counld be a half step that been interrupted.

**Invariant: everything must store and send**

## Session name
path, rendered in hierarchy.

## Tool
all tool call have extra parameter `discription: optional[str] = none`

```python
Channel := path
Result := {id: Channel, output: str, time: time, mactehd: bool, exit: int|none}
fn exec(
  bin: path = channel[id].bin, #summary
  args: list[str] | dict[str],
  input: optional[str] = none,
  id: Channel = "/{bin}/default",
  wait: optional[time] = none, #summary
  until: optional[str] = channel_default(),
  remote: optional[str] = <session's remote>,
  cwd: optional[path] = <session's cwd>) -> Result
):
  # create a channel running the binary at `bin` if `id` does not exists.
  # if `id` exists and `bin` arg doesn't match the channel's `bin`, report error.
  # when waiting timeout without util macthed, set the channel's wait flag.
  # when channel matched the util or finished, if there is wait flag, reset it and notice the session with `Result`.
fn wait(
  time: time, #summary
) -> time
fn <tool>(**args):
  # route to server cmd with caller session as extra argument
  # extra tool's schema is not registered in tools but dynamic found via skill.
```

- wait: if none, wait default time, if 0, return immediately.
- util: wait util macthed certain string, config default value for channel based on its name.
- id: in each session's namespace.
- output: new output since last read.
- time: time cost since last input.

## Server Cmd
sytax:
- `<struct>{..}`: struct update type, e.g. `{a: int, b: float}: {a: int, b: float, c: str}{..}`.

```python
Cmd := {name: str, args: dict[str, any]}
Session := path
fn new_channel(id: optional[Channel] = none, name: str, session: Session):
fn input_channel(id: Channel, input: str, session: Session)
fn rename_channel(id: Channel, new_id: path, session: Session)
fn ls_channel(path: path, session: Session)
fn del_channel(path: path, session: Session)
Model := {provider, model}
ModelConfig := {
  model: Model,
  thinking: Thinking = provider_default(), # re-default if model change
  ctx_limit = 70%, # re-default if model change
  cache_tier = provider_default() # re-default if model change
}
Workspace := {remote: optional[Remote] = none, cwd: path = "."}
SessionConfig := {
  model_config: ModelConfig,
  workspace: Workspace,
  keep_alive: bool, # if auto send message to interrup waiting when approaching the cache alive deadline.
}
fn new_session(id: Session, config: SessionConfig)
fn set_session(id: Session, config_delta: SessionConfig{..})
fn ls_session(path: path)
fn del_session(path: path)
  # also delete all channel under the session.
fn send_message(session: Session, message: str)
  # inject message when step end.
  # interrupt if tool waiting.
fn interrupt_session(id: Session):
  # for waiting tool call, make it early return.
  # stop llm streaming and loop, everything received must be saved to context.
fn trigger_event(event: Event)
Subscription := path
fn subscribe(id: option[Subscription] = none, pattern: Pattern, cmd: Cmd, session: Session):
    # when pattern matched triggered event, run the cmd.
fn del_subscription(path: path, session: Session)
fn ls_subsscription(path: path, sesssion: Session)
cancel(keep_alive: bool = false):
  # queue following notices.
  # for running sessions that are not waiting, set canceled flag to them, restart will auto continue session with cancel flag.
  # for running sessions that are waiting, set waiting flag to them, restart will continue their waiting for the lefting time.
  # interrupt every running session, then quit, by default kill all channel, if keep_alive, make every channel detached, retart will adopt them.
  # killing channel/failed to adopting channel will produce channel end notice.
```
## Python Cmd
provided by a shipped uv python package.

```python
Skill := path # abstract naming space
fn ls_skill(path: path, session: Sesssion)
fn run_skill(id: Skill, cmd: Cmd, sesssion: Session)
fn get_messages(sesssion: Sesssion, range: Range)
fn clone_sesssion(id: Sesssion, new_id: path)
```

## Server State
everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.

## Prompt
after the system prompt, before the first user message, inject one message with useful information about session name, harness version (commit id), cwd, time. it is stamped once with the session's first prompt row and never re-rendered.

**Invariant: context only has append/cut API**

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
- the summary replace the history to compact.

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

event :=
  | user_message
  | harness_injection
  | assistant_message_item{reasoning | toolcall | output}: streaming.
  | tool_result:
    - channel's output streaming.
  | session related cmd
  | session related cmd result
  | error

events (vertical) :=
  [
    |summary{
      symbol (colored), content (gray),
      |reasoning{beginning (line), ending (line)} (vertical)
      |toolcall{tool_name·[*summary_args]·description}
      |tool_result{ok, time}
      |harness_injection{beginning (line)}
    } (horizontal, click fold/unfold(show all non-summary information), block_size, inner scroll, horizontal_scroll_margin)
    | output: colored markdown
    | user message: symbol (colored), content (white)
    | cmd/cmd_result: symbol (colored), content (gray)
    | error(red)
  ] (vertical scroll),
  streaming: currently stream, block_size, scorll if block_size!=0

- scroll
  - if scrolled from bottom, do not auto scroll, show a go to bottom button.
  - an opened block never hands its scroll to the events: its rules and its `box_edge`
    columns are the events'. scrolling where the pointer is decides, once.

- hot-key
  - Shift+Enter: new line in cmd_input.
  - PageUp/PageDown (Fn+Up/Down): scroll the sticky session.
  - Shift+Delete: delete sticky session.
  - Ctrl+V: paste (text/image).
  - Ctrl+C: copy the selection.
  - Shift+Up/Down: last/next cmd input.
  - Ctrl+Q: exit.

## Sticky
stikcy argument will auto fill in missing argument in function call with corresponding type.
use argument with certain type will set the sticky value of that type.
use struct update type will also update the corresponding value of that type.


## Client Cmd
all named argument's name can be ommited via argument order.
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

## Remote
Auto detect, validate and install remote runner with latest compatible version.

## Git Managed Config
auto choose the latest commit within same protocol, personal(local) change rebase on official(remote).

**Invariant: ALL Config File Must Be Live Tracked**

## Skill
provider uv python package, API can be called by harness cmd or python import.

## Skill: Self Control
do whatever user can do to the server.

## Skill: Subagent
- create a new session under caller session's name.
- subscribe new session's turn end event.
- send_message to new session.
- optionaly wait.

## Skill: Evolve
restart server should still keep its lifetime unchange, e.g. tui spawned server's lifetime bound to tui.

## Skill: Polity
agents play certain roles to keep context focus.

roles:
- justice (pro/sota): talk to the user, record user's requirements.
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
- requirement{proposals, plans, reports, solved}
- proposal{example, expected, actual, actual_spec, expected_spec}
- plan
- report
- judgement over proposal/plan/report{spec violation: none | premise | direct}
- spec amendment{ref proposal, ref judgement}
