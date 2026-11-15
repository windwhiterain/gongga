# `crates/server/src/tools/mod.rs`

## tools (module doc)

The SPEC gives the model `bash{name, cmd}` to run a command in a named
persistent shell and `del_bash{name}` to throw one away; `check_bash` and
`list_bash` are the reading half of the same idea, and `del_bash` exists
because a `background` bash outlives everything else and therefore needs an
explicit way to be stopped. Shells are keyed `session_name/bash_name`, so two
sessions never share one and a single session can keep several independent
ones.

Nothing here drives the harness. There was a `harness` tool that called the
dispatch in-process, and a tool call cannot be piped, looped or written into a
script, so the agent's commands are a line it types in `bash` instead -- see
`server-shell-command.md`. `with_builtins` is the whole registry: the four bash verbs; `with_python` adds the persistent interpreter and the three file verbs
described in `server-tools-python.md`; `check_bash` carries the optional wait,
so there is no `wait_bash`.

## A row that names a command this build has no type for

SPEC's tools are the rows a home declares; `exec`, `wait` and `live_compact` are this
build's own. Every other `[tool.<name>] cmd = "<what>"` row is offered as the command it
names: the call's arguments are that command's arguments as the call stated them, and the
command layer runs it with the caller's session. The `cmd` key is dropped on the way
(`named_words`): in a row that key names the command, so it is never one of the command's
arguments.

The table is therefore the registry on both sides. A row may offer a `ServerScript`, a
skill's entry or a command added tomorrow, and the answer to a name nobody answers comes
from the command layer, in its words, rather than from this module saying "no such tool".

A row of this kind answers in this process and nowhere else: it is a command the
dispatch resolves, so a call that leaves the process could not reach it.

## `gongga_cli`: one tool, and the call names the command

SPEC's `fn gongga_cli(cmd: Cmd)` is the same door with the name taken from the call
instead of from a row: `cmd` is `{"name": "<command>", "args": {...}}`, `GonggaCli` reads
it, and `run_command` runs it -- the caller's session riding along, the command layer
answering for a name nobody has. Two shapes of one route, because a config cannot
enumerate the commands a skill teaches: a model told `gongga-cli '{"name": ...}'` by a
skill can call the same command as a tool, with no row and no schema stamped for it.

`cmd` is read by `route`: the object, or the words of one, since a call may spell a
table as its JSON. `args` is optional -- a command need take none -- and its values are
the ones the call stated (`words_of`), because SPEC's `args` is `dict[str, any]` and a
value is never its text. Anything else is refused by name rather than guessed at: a
route that ran a command the call did not mean would be read as a success.

## the rows a connection would have been sent

A server cmd's answer can be made of rows: `get_messages` and `events` return one event
per stored row, and `net.rs` writes them to the asking connection before the result. An
agent's tool call has no connection, so `run_command` puts the same rows in its own
result instead, under `events`, as `{"seq": n, "event": {...}}`: the event and its
number, never a rendering of it, so a model that parses them and a window that draws
them are reading one shape. The field is absent when the answer had no rows, which is
most commands.

It is not a second way for a client: a client's `cmd_result` still carries only `data`,
and its rows still arrive as `row` messages. It is the first way for the caller that had
none -- without it `get_messages` answered a model with "N row(s) at seq 1..40" and no
row at all, so a parent driving a subagent could not read what the child had said, nor
why its last turn died.

## Descriptions point at `bash` instead of repeating it

Every tool's description rides every request, so a sentence said twice is paid on
every turn. The non-local tools therefore state only their difference, in the tool's
own terms: `remote_bash` is `bash` on the peer,
`remote_python` is `python` as `remote_bash` is `bash`, `python` is `bash` with `code`
in place of `command`, and `check_python` / `del_python` / `list_python` name their
shell counterparts in one clause. Measured: the sentences that describe the two
machines went from 3391 to 2269 characters, with `check_python` added rather than
dropped.

A description is also what a config rewords (`[tool_text]`, see `server-tools-text.md`),
so a sentence a model misreads is fixed without a build.

A description says what the tool *is* and stops there. How the two machines relate --
one name space, which of them holds `default`, what a remote tool does not offer, what
to do instead of naming another machine -- is the `remote` skill's, because that is a
question a model asks and not a call it makes. The instructions a session receives
carry none of it, which is also what keeps a session without a remote from reading
about one.

The trade is a reference the model must resolve across the tool list. It can, because
every tool is visible in the same request; what it cannot do is follow a pointer into
a note, so no description defers to prose that is not itself a tool.

## ToolContext::detached

Deliberately not part of a shell: a background job outlives its shell, its
session and the server.

## ToolContext::explore

`Some` is explore mode and carries the session's workspace snapshot; `None` is work
mode. One field rather than two because there is no state in which they disagree: a
session that explores has a chain to compare against, and a session that does not has
nothing to compare.

It is read per call, not captured when the turn started, so `set_session --mode` reaches
the very next call. See `docs/notes/server-explore.md`.

## ToolContext::store

Where a command that outlived its deadline reports back to the session when it
finishes. That report is an inbox row, not a tool result: the call it belongs
to returned long ago, and the session may be idle by the time the command
ends.

## ToolContext::replace_shell

Every path that ends a shell for a reason the model did not ask for -- a
command that outlived its wait, output that lost its sentinel, a turn
cancelled mid-command -- does the same thing afterwards: the name has to keep
meaning "a shell you can use", because the tool results and `list_bash` say it
does. The pointer check inside `replace` is what keeps this from taking away a
shell somebody else already put there; `false` means the name now belongs to a
different shell.

## ToolContext::cap

Cap one command's output, spilling the rest to a file named after where it
came from: session, shell, `description`, call id.

The `description` is in there because a dump is read later, out of context -- a dozen
`...-default-call_00_abc.out` files say nothing, and the whole point of
keeping the full output is being able to find the right one again. `slug()` in
`bash.rs` is what makes it safe for a path.

## standing_dir

Where a program this machine starts stands when the session cannot say. The session's own
directory, when this machine has it; this server's own otherwise. The two ways a session's
directory is not here are an empty path -- its directory is on a peer -- and a path that
is no longer a directory: both are a place to start somewhere else, never a failed spawn.

A directory a *call* stated is not this function's business. `exec` resolves its `cwd`
first and refuses an unusable one by name, keeping whether it came from the session or
from the call, because the two sentences differ. See `server-tools-words.md`, "The
directory a program stands in".

## The wait, and the clock

`waited` is the only wait: a call's work runs on a task with a token of its
own, and the wait ends when the work answers, a message arrives, or the turn
is interrupted. It has no clock of its own -- `exec`'s `wait` and `wait`'s
`time` are how a call says how long it will wait, and a number here would end
waits nobody asked to end.

A timeout in this crate is therefore a tool's own: a bound it holds against an
execution it runs, to call a hung mechanism a fault. `CALL_TIMEOUT_MS` is the
one such number today, the skill program's. No caller sets it, no caller reads
it, and while the harness is correct the interface is unchanged by it.

## run_command (a command the harness cut)

The call is a `select!` between the command layer and the caller's cancel token, so a
cut can land before the command answered. Nothing in that state says whether the
command ran: an empty `Result` (`"exit": null`, no output) reads to a model as "ran,
said nothing", and that is how one `send_message` was sent twice. The answer is
therefore an error naming the cut and telling the reader to look at what the call
would have changed before repeating it -- the same rule as `BashTool::run (a cancelled
result)`: never let silence pass for success. The two ways a cut arrives are the turn's
own cancel and the cache tier's deadline (`server-agent.md`, "The tier's deadline"),
which no longer fires on a tool that has not started.

## Tool, Cmd::forwards_to

The pair answers one question nothing else can: is this row a command's spelling, or
a tool with work of its own? `Forward` says `Some(name)`, every other `Cmd` says
nothing, and `Specd` passes its work's answer up. `tool_cmd::catalog` reads it to
leave the alias out, and `Dispatch::catalog` reads it to keep a declaration from taking
a name this build implements.

## Tool

Implemented with a boxed future rather than `async_trait` so the crate keeps
one fewer macro dependency; the signature is the same either way.

## laws: the arguments every call carries

SPEC gives every tool call `description` and `max_output` beside the arguments its own
row lists. They belong to the call layer, and `tools::laws` is where that is written
down: one list of names, kinds and sentences, and every door reads it.

- **The schema a model is offered.** `general_parameters` renders a tool's schema by
  handing it to `laws::schema`, so a row that declares neither name still offers both. It
  is one call rather than a per-tool block, which is the whole point: a tool added
  tomorrow cannot forget them, and nothing in `tools/*.rs` names either one.
- **The check a call is held to.** `argument_error` passes a shared name over, because the
  schema it is judged against was rendered by the same list.
- **The reader that takes them off.** `Registry::run` calls `Given::take` before anything
  else sees the arguments, and applies `max_output` itself. A command would answer for a
  name its handler has no parameter for, and a skill's function cannot bind one at all,
  so taking them is what lets a new tool be added without knowing they exist.
- **The operator's line.** `tool_cmd::catalog` appends them to every tool command, so a
  person types what a model spells.
- **The command doors.** `gongga_cli`'s route and `declared_command` are the two other
  ways a call reaches a handler, and each reads the same list for the same reason: a
  declared command's function is a Python signature that never declared the name.

Why not a parameter on each tool: the failure being fixed is that a new tool is written
against its own row and silently drops the two names SPEC says every call takes -- the
model still writes them, and the call comes back "takes cwd, limit, offset, path,
remote". One list makes that unrepresentable rather than a thing to remember.

A shared name is still a schema name: `laws::schema` writes it under `properties` with its
kind, so a provider's validator accepts what the call layer reads. `null` comes out of
every kind, because a provider compiles a union only when the other branch is a string --
handed `["integer","null"]`, `qwen4-max-preview` wrote the string `"120000"`, and handed
`["array","object"]` it wrote the JSON text of the array. A model is offered the kind
itself: one that wants the default omits the argument, and `none` is how it names this
machine. The check keeps `null`, because a client may still state it.

## argument_error

Assembled from [`Tool::parameters`] rather than written per tool, so it cannot
drift from what the model was handed: which arguments exist, their types, which
are required, and which declared name a wrong one was probably reaching for. A
hint that has to be maintained by hand is a hint that goes stale the first time
an argument is renamed.

Only a *missing required* argument rejects the call. A stray key on its own is
passed through, exactly as it was before this existed: a call that would have
run is a call the operator wanted, and failing it over an ignored key would
manufacture the retries this message exists to prevent.

## is_shorthand_of

`cmd` is inside `command` and `command` is inside `cmdline`, so direction does
not matter: a name the model invents for an argument is nearly always a
shortening or a spelling-out of the declared one, and naming the declared one
back is what turns a rejected call into a corrected one. This is a rule over
the schema, not a list of synonyms, so a renamed argument never needs a new
entry -- and a key unrelated to anything declared gets no guess rather than a
wrong one.

## INBOX_POLL

A tick rather than a notification. Notifying every waiter needs somewhere to
keep them, and the number that matters is not "did one arrive" but "did one
arrive for *this* call", which is a per-call question: the call answered it by
comparing a copy of the count it took when it started. A quarter of a second is
well below anything a reader notices, and the cost of the tick is one load.

## ToolContext::inbox

Where the signal comes from: the session owns the counter, and every tool call
in a turn takes a fresh copy, so a message wakes exactly one wait instead of
every call that follows it.

## Cancelled, and why it is gone

It was its own type rather than a string because the cancel path had work to do
that no other error did: the command had to be stopped, and the shell it was
running in thrown away. Now the tool body is still there when the wait ends -- SPEC's
"make it early return" -- so the tool does that work itself, on its own path, and no
error type is needed to tell a stopped call from a refused one.

## BashTool::run (the `command` argument)

The argument is `command`, not `cmd`: a model reaching for the word it already
knows is right far more often than it is wrong, and the schema should be the
thing that bends. A missing one is rejected by `argument_error` first, in the
schema's own words; what is left here is a `command` that is neither a string nor a
list of strings.

## command_line

`bash` takes a command line, and a line is read by a shell: its words are split,
expanded and globbed, and the quoting that says which is which is the model's to
write. A program and its arguments, where an argument carries a space or a
quote, belong to `exec` instead -- see `server-tools-exec.md`.

## A word holding a NUL

Refused by `exec`, because it cannot arrive as the word that was sent: an argument
ends at a NUL for every program there is, and the byte is dropped somewhere on
the way. The agent would get a word it never wrote -- a different word, silently
-- so the call is refused instead, and the refusal says why.

## BashTool::run (the `description` argument)

Two readers, one string: the call's row (SPEC's `toolcall{tool_name·description}`)
and the spill file's name. Only ever a label either way, so a missing or
nonsensical one costs an untitled row and a less informative path rather than a
failed call -- which is why it is read here and not validated.

## BashTool::run (a background job's cwd)

Only the working directory is inherited. Exports and shell functions are not,
because a background job does not run in the shell -- it is a process of its own.

## BashTool::run (relaying output)

Relay shell output to `tool_answer` events while the command runs. Draining has
to be concurrent with the run itself: if we awaited the command first, a
chatty command would fill the channel and block the shell.

## BashTool::run (the scoped future)

Scoped so the future borrowing `shell` is dropped before the guard is, which is
what lets the guard go out of scope at all.

## BashTool::run (a cancelled command)

A cancelled command is stopped, not abandoned. Dropping the future only stops
us reading: the shell keeps the command, and would answer the *next* call with
its leftovers (and with a success status that belongs to it). Killing the
process group is both the "turn cancel interrupts the foreground command"
contract and the only way to know the shell has no command boundary owed.

## BashTool::run (reading `usable` under the guard)

Read while the guard is still held: a shell that lost its output stream must
not be handed another command, and this is the last moment its state is
visible without taking the lock again.

## BashTool::run (still running after the wait)

The deadline was on waiting, not on the command. Nothing was stopped: the shell
still holds it. Give the session a fresh shell under the same name so work can
continue, and have the old one report the rest of the output into the inbox.

## A message arriving mid-call

This is the fix for "the session went quiet for five minutes while I was talking
to it". A step is one model request *plus* every tool call it asked for, and the
inbox is read only between steps, so a call that waits for its whole `timeout_ms`
-- 300 seconds, the way it was configured that one time -- holds whatever the
operator said until the deadline arrives. The turn was not stuck; it was in the
middle of a step that nobody could shorten.

The first version of the fix was the `bash` body's own: a three-way `select` over
the command, the cancellation token and a 250 ms inbox tick. That was one tool's
answer to a question every tool has. `exec` and `python` have the same question
and no answer -- a `remote_exec` waiting four hours for a file to appear could
not be reached by `send_message` *or* by `interrupt_session`, and the operator's
message sat in `pending_tasks` until the timeout it was written against.

So the wait is `waited`, in `ToolRegistry::run`, and it is the only one: every
tool is awaited there, the inbox is compared against the copy the context took
when the call started, and the work's own token is what says the wait is over. A tool
that watches it answers with what it has and never sees an error; one that does not is
aborted, which is what makes a program die with its call (`kill_on_drop`, or the peer's
connection closing) and what frees a shell's lock for the task that reads the rest of
its output.

## a waiting call returns early

SPEC's `interrupt_session` asks one thing of a call that is waiting: "make it early
return". So nothing answers in a body's place. The body is still running when the wait
ends, it is told so through the token it already had, and it returns the answer it would
have given a moment later.

Three tools wait, and each one's early answer is the answer it already had:

- `wait`: SPEC gives it `-> time`, so it answers with how long it waited -- its own
  deadline and an interrupt are the same answer, because an ended wait is an ended wait.
- `exec`: a wait that ends early is an `exec` that waited for no time at all, so
  `talk` answers with the window on disk right now, as SPEC's `Result`.
- a command route (`Forward`, and `GonggaCli` with it): the command said nothing, so its
  `Result` carries an empty `output`, `exit: null`, and the time it spent.

A tool with no wait to end -- a file read, a skill's entry -- is aborted, and `stopped`
says the work stopped before it could answer. That is the only sentence left about a
call's ending, and it is a fact about the call: no tool learns which verb ended the
turn, because nothing it answers may depend on that.

What the stop does to the turn is `commands`' business: whether a restart continues it,
and what a window is told, are decided there and read from `StopSignal`, not from
anything a tool call says.

## BashTool::run (the late spill label)

Same naming rule as the foreground path, with `late` standing in for the call
id: the call it belonged to has already returned, so its id is no longer what
identifies the run.

## BashTool::run (the late lock)

Nobody else holds this shell any more, so waiting for the lock is waiting for
the call that timed out to return -- which has already happened.

## BashTool::run (a pipe without its sentinel)

The pipe ended without its sentinel, so this shell cannot tell one command's
output from another's. It is replaced here rather than on the next call, so the
model never gets to send a command to a shell that would answer it with the
previous command's text.

## BashTool::run (capping before storing)

Capped before it is stored: what goes into the transcript is replayed on every
later step, so the full output of a chatty command would be paid for over and
over.

## BashTool::run (a cancelled result)

Same treatment as lost output: the shell may still be holding part of a command
that was stopped mid-flight, so it is never handed another one. The result says
what happened instead of pretending the command finished -- the model must not
read a cancelled command's silence as success, and it must not expect its
output.

## BashTool::run (a failed write)

The write itself failed, which can leave the shell with half a command line in
its stdin. `healthy` is what says so; dropping it here is how the next call
ends up reading this one's leftovers.

## WaitTool

SPEC's `wait`. It waits for whatever the session has waiting -- a background job's
report, a `subscribe` match, a person's message -- and it consumes none of it.

That last part is the whole design. A tool that drained the inbox would become a
second delivery path: the same news would arrive once in a tool result and once as
the message it really is, and the step boundary's `deliver_pending` would find a row
already read or, worse, missing. One road, so `wait` only reports *that* something
landed, names it in a few words, and lets the boundary deliver it.

Two sources, because neither is enough. A person's message bumps the session's
`InboxSignal`, which a queued row does not; a background job's notice and a
subscription's news are rows in the inbox, which the signal does not see. Polling
both is cheap -- one indexed read per `INBOX_POLL` -- and it is the only place where
"what has arrived" and "what has been delivered" can be told apart.

Only `inject` rows count. A `turn` row is the turn's own obligation: written before
it starts, deleted when it finishes. Counting it would make `wait` return at once,
every time, on a session that is simply working -- which is exactly the bug the first
version of this tool had.

The ceiling it defaults to is `bash`'s, because that is this harness's one answer to
"how long may a tool hold a turn"; the deadline passing is not an error, and the news
is not lost. The tunnel's cap does not apply to it -- waiting is what it is for -- so a
`wait` may legitimately ask for longer than any other call.

A `cancel` ends the wait, but keeps its turn: SPEC says a wait lives across a cancel
and is still waiting after the restart. The call marks the session while it is inside
`wait` (`StopSignal::begin_wait`, cleared by the guard's drop even when the call is
aborted), and `cancel` reads that mark before it fires the token. A marked turn is
`mark_resumable`, its `turn` row is kept, and `finish_turn` preserves it -- so the
next start resumes the turn and the wait begins again. `interrupt_session` and
`del_session` pass `keep_waiting: false`: there "stop" means "drop it".

Every other call is still interrupted where it stands. A wait is the one thing a
cancel keeps because it started nothing; keeping a turn that was in the middle of a
program would run the program twice.

## ListBashTool::run (detached rows)

Background jobs belong to the session, not to a shell, so they are listed on
their own -- including ones adopted from a previous run of the server, whose
shell no longer exists.

## DelBashTool::run (an unanchored pattern)

Unanchored on purpose: `qa` is how someone actually names a shell family
(`qa`, `qa2`, `qa5`), and requiring `^qa` would make the cleanup of a long
session's leftovers guesswork.

## DelBashTool::run (surviving background jobs)

Background jobs are not the shells', so they keep running; say so, or the model
would assume discarding the shells stopped them.

## a name is one shell, on one machine

`bash` and `remote_bash` are one implementation driven twice: `BashTool { machine }`.
A session with a remote is offered both; a session without one is not offered the
second, because a tool that can only refuse is a wrong turn nobody can undo.

`check_bash`, `del_bash` and `list_bash` carry no machine: a name is one shell, and
whichever machine holds it is the one that answers. `list_bash` reports it per row and
`check_bash` in its closing bracket, so a reader can tell `peer` from `here` without
asking. A name already taken on the other machine is refused, naming the machine that
has it and asking for a different `name`: `default` is the directory-owning machine's,
so a remote session's local `bash` hits this on every call that names none, and the
first thing that refusal has to say is which parameter to change. `del_bash` is
mentioned second, because it frees the name by ending a working shell.

The directory a shell starts in follows the same rule as the session's own: the
machine that owns the placement gets it, and the other side starts in its own default
directory. `default` is therefore the directory-owning machine's shell -- so a remote
session's `default` is the peer's, and `bash` there means *name another shell*.

## `remote_bash` (the `target` argument)

SPEC declares `target = session's remote`, and the argument is what a model writes when
it is following the spec rather than this schema. `argument_error` passes a stray key
through rather than failing the call (see above), so an undeclared `target` was a key
that silently did nothing: the command ran on the session's peer while the model
believed it had chosen a machine. Declared, it is refused unless it names that peer,
and the refusal names `set_session --remote`.

Only one value can be accepted, because the other machine would have no directory to
stand in: the session's cwd is the peer's path, and a reference printed by a `bash`
call on a second peer would be read through the first one's connection.

## no remote word reaches a session that has no remote

`definitions(has_remote)` drops two kinds of thing at once: the tools whose twin drives
the peer (`Tool::is_remote`), and the `remote` argument a file tool takes. A session
without a peer is told nothing about one -- not a tool it cannot call, not an argument
that can only be refused, and not a sentence in a description about "the other
machine", which is a fact it can act on in no way at all.

A session that does have a peer is told what each `remote_*` tool is and nothing
about the relationship between the two machines: what is shared, what is refused and
which machine holds `default` are in the `remote` skill.

The tool answers for itself because the list of names drifted once: `remote_python` was
added to the registry while `is_remote_tool` still named only `remote_bash` and
`remote_exec`, so a session with no remote was offered an interpreter that could only
answer "this session names no remote". A turn was spent on it, and the model went on
believing a remote tool existed under some other name. What a session can learn about
another machine now comes from the `remote` skill, which is prose it asks for and not a
call it fails.

## the schema is per machine

`BashTool::parameters` builds the two fields only one machine has: `background` on
this machine's `bash`, `target` on the peer's `remote_bash`. A flag a machine cannot
honour is not a flag the model should be shown -- and both are still refused at run
time, because the refusal is what explains *why*, and the schema is what keeps the
model from asking.

## the machine a spilled cut belongs to

A capped output is spilled to a file this server can name only when the shell that
printed it runs here; over a remote the path would name a file the reader cannot
open, so the answer is reported without one.

## spill_label

One function rather than two `format!`s because the two paths that spill -- a
foreground result, and the late output of a command that outlived its wait --
must produce names that sort and read the same way. The `task`, when the model
supplied one, is what makes a dump identifiable later; without it the name is
still unique, just less useful.
