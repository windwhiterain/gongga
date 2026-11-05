# server/subscriptions.rs

## subscriptions

SPEC's `subscribe`, as one registry and one matcher.

## What a subscription is

Four facts: a *name*, a *pattern*, the *session that runs the match*, and the
*command* a match runs there. It is armed as `subscribe <name> <pattern> --bash <shell>
--cmd <cmd>`, re-arming a name replaces what the name held, and `del_subscribe <name>`
drops it -- a name is one subscription, so the two verbs are always one well-defined
row. The command is what a match *does*: the server no longer speaks for a
subscription, so a row armed without one fires and does nothing. SPEC still spells the
action as optional (`[--bash <bash> --cmd <cmd>]`), so arming one without an action is
allowed and warned about rather than refused -- narrowing the surface is the SPEC's,
not this module's. The pattern is a regex, matched against an
event's name -- which for a log row is its `kind`, and for any other event is its
own (`status`, `usage`, `sessions`, `log`, ...). When the event is about a session,
the pattern is matched against `name:session` too, so `turn_end:child/2` is one
session's turn ending and `^turn_end$` is any session's. `trigger_event <name>`
publishes an event under a chosen name, so a subscription can be fired by hand.

The same language `del_bash` and `list_bash` speak, for the same reason: a name
filter that is a regex needs no lookup table, and an unanchored match is the
convenient default rather than the only possibility. A pattern that cannot compile
is refused *before* the row is written: a subscription that can never fire is worse
than no subscription, because it looks armed.

## Why it does not wait

`subscribe` answers at once. It used to be the other way round: a client command
`wait --kind K` read the connection until a row matched, which made every caller a
process parked on a socket. Two things were wrong with that.

The first is the address. `wait` printed the row to whoever ran it, so a *session*
could not be told anything by it -- the model that wanted the news had to poll, or
spawn a shell and read its output. SPEC's `subscribe` is the answer to that: a match
runs a command, and the command is free to be the one that tells a session.

The second is that waiting is a turn, not a process. An agent that wants to stay
where it is until something happens calls the `wait` tool: it costs part of one
turn, it is cancelled with the turn, and it is visible in the transcript. A shell
that sleeps is a shell nobody can see.

## The server does not speak for a subscription

An earlier build queued the news itself -- a match became an `inject` row in the
subscriber's transcript, and the action was an extra. That was never in the SPEC the
current commands are read against: the `notice the caller session` wording belongs to
the older `subscribe --event <pattern>`, and it was replaced, not narrowed, when the
action arrived. It is gone from the code now, for two reasons.

The first is that it made the action half a mechanism: two ways for one match to
reach a session, and the one the skills documented was the one nobody had to write.

The second is durability. A notice was one `INSERT` -- cheap, and it survived a stop
because the row *is* the obligation. Carrying the same news through an action costs a
process and one round trip, and it survives a stop only because the two things below
hold. Both are worth paying only because the alternative is a server that speaks for
statements it was not asked to make.

- **The matcher outlives the ask.** A stop request arrives minutes before the last
  turn ends, and a turn that ends *during* the drain is exactly the news a subscriber
  is owed. The matcher therefore stops at the end of the drain, not at the ask.
- **An action holds a slot in the drain.** It is counted where a turn is counted, so
  the process waits for it. An action that says what happened -- `send_message` --
  then lands as a pending row while the server is stopping, and the next start
  delivers it like any other owed message.

An action that fails leaves one `subscription` row with `ok: false` and the reason.
That is the whole of what a lost notice looks like now, so the row names the
subscription, the session, the event and what the command said.

## What the action is told

A bash action runs as one line of the session's own shell: a subshell, whose first
three statements are the event, and whose last is the action's command.

    ( GONGGA_EVENT_KIND='turn_end'; GONGGA_EVENT_SESSION='main/impl'; GONGGA_EVENT_PAYLOAD='{…}'; <cmd> )

Two shapes were wrong before this one, and both were silent.

`VAR=x cmd "$VAR"` does not work: a simple command's words are expanded *before* its
own assignments take effect, so the action reads nothing and looks like an event with
no data. Hence three statements and `;`, not one prefix.

Assignments without the subshell would work and then stay: a callback would leave
three stale names in the shell the session works in, and the next honest `bash` call
there would read last hour's event. The subshell also leaves the action's exit status
as the line's own, which is what the `subscription` row records.

The values are expanded, never re-parsed, so a payload full of quotes is carried as
data. A subscriber writes its command once, before it knows what will fire, which is
why the three names are fixed rather than templated.

A python action is told nothing: `--code` is one string in a REPL, and an assignment
prefix would stay in that interpreter afterwards, so the event is left to be read
from the row instead.

## The caller

The session a subscription notices is the one the command was run *from*, read off
`$GONGGA_SESSION` by the client and sent as `ClientInfo::session`. It is deliberately
not the `session` argument: that one is who a command acts on, and `subscribe` is the
one command where the two are different questions. A client that names no session --
a window, a bridge -- is refused rather than armed with no addressee: a subscription
that fires and tells nobody cannot be noticed by the registry.

## A match can also run something

`--bash <name> --cmd <cmd>` (or `--python <name> --code <code>`) is the action, and a
match has nothing else: the command runs in the session's *own* persistent interpreter
of that
name -- the same shell the `bash` tool uses under the same key, so a subscription
and its session see each other's `cd` and exports. The name is the interpreter's,
not the subscription's: the subscription already has one name, and the interpreter
it runs in is a place, not a label. The outcome is written as an event-log row of
kind `subscription` -- stdout, stderr and exit code -- so `events` reads what the
watcher saw. Running the action takes the same `tool_cmd::run` path the tool
commands use, which is what keeps a subscription's run and a model's call from
drifting apart. The name it runs under is the subscriber's choice, and a callback is
worth its own shell: the same name is the same queue, so a callback behind the
parent's long command is news that arrives late.

## What a wake looks like now

There is no `inject` row a match writes any more, so what a subscriber wakes to is
whatever its command sends. The `subagent` skill's command is `send_message` to
`$GONGGA_SESSION`: idle, that starts a turn; busy (inside `wait`, say), it queues an
`inject` row the next step boundary reads -- the same two roads as before, one hop
longer.

## Life of a subscription

- `--persistant true` keeps it armed after it fires; without it one match is the
  whole subscription, and the row is deleted as it fires.
- A row carries the action it was armed with, so a restart resumes both the
  watching and the running; the named interpreters it runs in are the session's
  own, which resume with the session.
- `del_session` drops a session's subscriptions: a deleted session has nobody left
  to tell, and a later session of the same name must not inherit them.
- There is no deadline and no cancel verb. A subscription is a statement about what
  a session wants to hear, so it lasts as long as the session does; the two ways it
  ends are "it fired, once" and "the session is gone".
- The rows are in the database, so a restart resumes them; the in-memory list is a
  compiled copy, not the record. A row whose pattern no longer compiles is dropped
  from both, rather than failing on every event of every start.

## One matcher

Matching happens in one task subscribed to the event bus, not at each publish site.
The alternative -- publishing sites consulting the registry -- would make every
`log_event`, every `set_status` and every streamed chunk pay for a feature most of
them never meet. It also keeps the invariant on the other side of the bus intact:
the row is on disk before the notification, so a matcher that reacts to a `Record`
can always read the log row it was told about.

The task lags like any other subscriber, and says so: a matcher that missed events
cannot know which patterns they would have matched, and silence would look like
"nothing happened".
