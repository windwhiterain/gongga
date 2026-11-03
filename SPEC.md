# Gongga Harness
-- Rust Coding Agent Harness --

## Crate Structure
all crates has name "gongga-xxx", alias with name "xxx" in workspace manifest, live inside directory "crates/xxx". 
- protocol
- skill: where a skill is on disk -- the three roots, their precedence, the frontmatter,
  and the entry a body runs. No dependency, because the server and the command layer
  both read it and neither may link the other.
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
all tool has optional `description` argument.

use `which("bash")` as bash.
- bash{command, name = "default", background = false, detached = false}:
  cwd is session's cwd if session has no remote.
  runs in the persistent shell `name`, or starts a detached bash of that name.
  - key = {session_name, bash_name}.
  - background bash ended will notice its session.
- list_bash{name_regex}: list matched bash with the caller session's name in key.
- check_bash{name = "default", wait_ms = 0}: return bash's output that hasn't been read by the caller session. With `wait_ms > 0` and background jobs still running, wait until they finish or the wait expires, then return the unread output: a timeout leaves the jobs running.
- del_bash{name_regex}: delete matched bash with the caller sessions' name in key. 
- remote_bash{command, name, target = session's remote}:
  cwd is session's cwd if session's remote is target.
  reference the remote skill.

File tools run through the Python tool's interpreter on the session's machine, so
file content is a JSON field and never a shell command or Python source:
- read_file{path, offset = 0, limit = 0}: lines `offset..offset+limit`; `limit = 0`
  reads to the end. Capped like other tool output.
- write_file{path, content}: create parent directories and replace the file
  atomically; `content` is written as UTF-8 bytes.
- replace{path, old, new, all = false}: literal replacement; without `all`,
  exactly one occurrence must match, and any other count is an error. Atomic.


## Server State
everything in database (sqlite) under a home directory, structured for agentic retrieval/mutation.

## Prompt
after the system prompt, before the first user message, inject one message with useful information about session name, harness version (commit id), cwd, time.

### Invariant
transcript that has been sent must not be change.

## Session Name
a name is `/`-separated segments: `a/b/c`. no segment may be empty or begin with `@`.
a name is one row of the list, and each prefix of it is a group row.

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
  - Shift+Down: scroll the sticky session to bottom.
  - Shift+Delete: delete sticky session.
  - Alt+V: paste (text/image).
  - Shift+Up: last cmd input.


## Cmd
- server:
  - `set_session --session (sticky) <name> --model (sticky) <provider/model> [--thinking_effort <level>](initial: highest in supported or default if unkown) [--context_limit <n%>](initial: "70%") [--cwd <path>](initial: ".") [--remote <none|name>](initial: none) [--must_create <bool>](initial: false)`:
    - plus `set_sticky_cmd_argument --name session --value <session>`
    - `--must_create true` refuses the command when the session already exists: a caller
      that starts its own session must not join another one's transcript by reusing a name.
  - `del_session --session <name>`: also delete all bash with the session name in key.
  - `send_message --session <name> --message <string>`: inject message instantly, turn waiting bash to backgound.
  - `interrupt_session --session <name>`
  - `cancel`: not instantly, but finish every task that can't be serialized to data, then store the pending tasks into data, the exit. next start will auto pick up those tasks.  
- server_script: that registered by external python scripts
  - `get_messages --session <name> --range <begin:end>`: return the rows in that closed range `range` (negative number to count from end).
- client:
  - `set_sticky --name <name> --value <value>`:
  - `del_sticky --name <name>`
  - `set_render [--block_size <n>](initial: 8) [--box_edge <n>](initial: 2)`:
    - `block_size`: max line count of non-output block, auto scroll.
    - `box_edge`: columns at each side of an opened block where the wheel scrolls the
      events instead of the block; the block is drawn there like anywhere else.
  - `set_render_session --session <name> [--show <bool>]`
  - `connect_server [--ip <ip>](default: localhost) --port <port>`
  - `serve --port <port> [--home <path>](default: "~/.gongga")`
  - `skill <name> [args...]` | `skill ls` | `skill which <name>` | `skill run <name>`:
    run the entry a skill declares, resolved by the roots above. argv is passed through
    unparsed, the entry's exit code is this command's, and neither needs a server nor a
    runtime -- an entry is a file on this disk. `run` reaches a skill whose own name is
    `ls` or `which`.

## Skill
a skill is a directory holding `SKILL.md`: `name`, `description`, and what its body needs.
use python cripts read/write gongga home, calling server cmd.

three roots, and the first one to hold a name wins: the session's checkout
(`<cwd>/.agents/skills`), the home (`<home>/skills`, installed from the pin), the machine
(`~/.agents/skills`). A name found in more than one is reported as shadowed, never
resolved silently.

- `metadata.entry: <file>` in the frontmatter names the program the body runs, relative to
  that skill's directory. It sits under `metadata`, which the Agent Skills specification
  defines as an arbitrary mapping, so a skill carrying it stays valid elsewhere.
- a body runs its entry from **its own directory** -- `SKILL_DIR=<dir of SKILL.md>` then
  `"$SKILL_DIR/<entry>"` -- never from an install path. The anchor costs nothing here:
  `@skill:` expansion and `index_skill` both hand out the `SKILL.md` path.
- a skill with no entry is a body and nothing to run.

### Invariant
one reading of the roots, their order and the frontmatter: `skill`, used by the server's
table and `@skill:` expansion, by the command layer's completion, and by the entry runner.
A second walk is a second answer to "which file does this name open".

## Git Managed Config
personal(local) change always rebase on official(remote).


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