# server/subscriptions.rs

## subscriptions

SPEC's `subscribe`, as one registry and one matcher.

## What a subscription is

Four facts: a *name*, a *pattern*, the *session that runs the match*, and the
*commands* a match runs there. It is armed as `subscribe <pattern> <cmds>`, re-arming a
name replaces what the name held, `ls_subscription <path>` lists what a session holds
and `del_subscription <name>` drops it -- a name is one subscription, so the verbs are
always one well-defined row. A call that names no subscription is given the first free
`sub<n>`, because SPEC makes the name optional and a row nobody can name is a row
nobody can drop. The commands are what a match *does*, and a call that names none is
refused: a subscription that fires and does nothing is a promise with no keeper. The pattern is a regex, matched against an
event's name -- which for a log row is its `kind`, and for any other event is its
own (`status`, `usage`, `sessions`, `log`, ...). When the event is about a session,
the pattern is matched against `name:session` too, so `turn_end:child/2` is one
session's turn ending and `^turn_end$` is any session's. A turn that was cut
publishes `turn_interrupted`, not `turn_end` (see `server-agent.md`), so a
subscriber that must hear every ending writes `^turn_(end|interrupted)$`. `trigger_event <name>`
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

- **The matcher outlives the ask.** `stop` is pulled only after `serve` returns, not
  when `cancel` is asked, so events published during the shutdown-notice window still
  reach a subscriber. `matcher.await` waits for that loop to end, not for the actions
  it spawns.
- **An action's news is a durable row, not a wait.** The action runs as a detached
  task the stop does not wait for. An action that says what happened -- `send_message`
  -- lands as a pending row, and the next start delivers it like any other owed
  message; that row, not the task, is what survives the stop.

An action that fails leaves one `subscription` row with `ok: false` and the reason.
That is the whole of what a lost notice looks like now, so the row names the
subscription, the session, the event and what the command said.

## What a command may name

A match is not the only thing that knows what happened: the event does, and a command
written before the event exists cannot type it. So a **string** argument may name a
field of the event that matched, and the server puts the value there before the command
runs:

    send_message {session: main, message: "/impl ended its turn ({event.outcome}):\n\n{event.answer}"}

Three rules, and each of them is the version that cannot surprise a reader.

- **A record's payload is lifted to the top level.** A log row is a `kind` plus free-form
  fields, so `turn_end`'s answer is `{event.answer}`, never `{event.payload.answer}`: the
  fields are what a subscriber came for, and the envelope (`id`, `seq`, `kind`, `at`) is
  still there for anyone who needs it. `{event.name}` and `{event.session}` are added to
  every event, so an action need not know which kind it watches to say whose it was.
- **Only that spelling is a reference.** `{event.` starts one, `}` ends it, and every
  other brace and word reaches the command as written -- a message that is JSON, or that
  mentions `{a: b}`, is not rewritten. An unterminated `{event.` is text, not a refusal.
- **A path the event does not have is a refusal.** The alternative is a message that
  quietly reads `{event.answer}` where the answer should be, and a notice nobody can
  tell apart from a notice that said nothing. The refusal lists the fields that exist.

This is why the answer rides `turn_end` whole (see `server-agent.md`): a subscriber that
must fetch the rest of a truncated answer would be a notice one round trip short.

## The caller

The session a subscription notices is the one the command was run *from*, read off
`$GONGGA_SESSION` by the client and sent as `ClientInfo::session`. It is deliberately
not the `session` argument: that one is who a command acts on, and `subscribe` is the
one command where the two are different questions. A client that names no session --
a window, a bridge -- is refused rather than armed with no addressee: a subscription
that fires and tells nobody cannot be noticed by the registry.

## A match runs its commands in order
The action is a list of commands -- SPEC's `Cmd`, a name and its named arguments -- and
each runs through the same layer a tool's `fn <tool>(**args)` uses: the catalog decides
what exists, refuses what is unknown, and fills the caller's session into the first
argument that is one. So a subscription cannot reach past what the session that armed it
can call, and a new command is available to subscriptions the day it is added.

It is a **sequence, not a batch**: the first refusal stops the rest, so a later command
may count on the ones before it having run. The `subscription` row says how far it got
(`ran`) and what stopped it, so a match that died halfway does not read like one that
finished.

It is stored as one column, `action`, holding `{"cmds":[{"name":…,"args":{…}}, …]}`; the
row is the record, so a restart resumes both the watching and the running. A row armed by
an earlier build holds one command object instead of the list, and reads as a list of one
-- a home that was running before this existed keeps its subscriptions. The outcome is
written as an event-log row of kind `subscription` -- whether every command answered,
what the last one said, and which commands there were -- so `events` reads what the
watcher saw.

A match does not wait for anything of its own: the command it runs is what waits, if it
waits at all, so the shape of a wake is whatever that command does.

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
