# server/subscriptions.rs

## subscriptions

SPEC's `subscribe`, as one registry and one matcher.

## What a subscription is

Three facts: a *name*, a *pattern*, and the *session* that hears about a match. It
is armed as `subscribe <name> <pattern>`, re-arming a name replaces what the name
held, and `del_subscribe <name>` drops it -- a name is one subscription, so the two
verbs are always one well-defined row. The pattern is a regex, matched against an
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
spawn a shell and read its output. SPEC says `subscribe` notices *the caller
session*, which makes the news an inbox row rather than a line on a terminal.

The second is that waiting is a turn, not a process. An agent that wants to stay
where it is until something happens calls the `wait` tool: it costs part of one
turn, it is cancelled with the turn, and it is visible in the transcript. A shell
that sleeps is a shell nobody can see.

## The caller

The session a subscription notices is the one the command was run *from*, read off
`$GONGGA_SESSION` by the client and sent as `ClientInfo::session`. It is deliberately
not the `session` argument: that one is who a command acts on, and `subscribe` is the
one command where the two are different questions. A client that names no session --
a window, a bridge -- is refused rather than armed with no addressee: a subscription
that fires and tells nobody cannot be noticed by the registry.

## A match can also run something

`--bash <name> --cmd <cmd>` (or `--python <name> --code <code>`) carries an action:
on a match, the command runs in the session's *own* persistent interpreter of that
name -- the same shell the `bash` tool uses under the same key, so a subscription
and its session see each other's `cd` and exports. The name is the interpreter's,
not the subscription's: the subscription already has one name, and the interpreter
it runs in is a place, not a label. The outcome is written as an event-log row of
kind `subscription` -- stdout, stderr and exit code -- so `events` reads what the
watcher saw. Running the action takes the same `tool_cmd::run` path the tool
commands use, which is what keeps a subscription's run and a model's call from
drifting apart. The session notice below still fires either way: an action adds
what runs, it does not replace who hears.

## A match is a message, not a notice

The row a match writes is an ordinary `inject`, not one of the marked notices. That
is the difference between "news for a session that is already working" and "this
session asked to be woken": an idle session that subscribed is started, and a busy
one reads it at its next step boundary. Both roads end in the transcript.

The row carries `subscription: <id>` so the tool that reports arrivals can say a
`subscribe` match is what landed, rather than "a message".

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
