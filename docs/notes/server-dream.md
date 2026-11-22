# keep_alive：服务端只问，模型决定

SPEC's `## Keep Alive`: a turn end arms a timer. When the cache window enters the
dangerous band, the server sends one notice and stops there. The receiving agent
decides whether to spawn a dreamer, hand off, or mute the timer. The server no
longer forks a dream and no longer hands off on its own; the model owns both
decisions through `@skill(dreamer)` and `@skill(handoff)`.

## The timer

`arm_keepalive` (commands.rs) still owns the chain: one sleeper per turn end,
armed at the moment the cache was warmed. It sleeps to the lower edge of
`cache_fire_after_ms(model, tier)` and fires only when the elapsed time is inside
`[lower, upper)`. No usage row is read to move the window, and a session busy,
muted, or owning a legacy dream arms nothing. A generation change (a new
checkpoint), a mute change, or a new prompt retires an older sleeper at fire time.

A fire writes a `keepalive_notice` event and delivers `keepalive_notice()` -- a
`[prompts]` template with a fallback for homes whose content predates it. The
notice names the three choices and nothing else happens server-side: the agent's
turn starts from the notice, and what it calls is its own decision.

## Mute

`keepalive_handoff_at` is still the durable mute. `@skill(dreamer)`'s
`mute_keep_alive()` writes it through the `mute_keepalive` command; the same
command with `mute=false` clears it. Every successful `send_message` to the
session clears it too -- a message is a new prompt, whoever sent it. That is why
there is no `client` flag on the greeting any more: the distinction between a
window and an RPC line never changed this rule.

A muted session's next turn epilogue arms nothing. The next message starts a turn
and, at its end, a fresh timer.

## The dreamer

`@skill(dreamer)`'s `dream()` clones the caller with `clone_session --dream_of true`
into `<caller>/dream`, taking `/2`, `/3`, ... while a name is held -- the same
free-name rule the subagent skill uses, so two dreams of one session never share a
transcript -- mutes the caller, and sends the clone a dream
notice that starts its turn. The `dream_of` mark is what makes the copy disposable:
the epilogue discards it at its true turn end. The notice tells the dreamer to
organize `.agents/notes/` and `docs/notes/`, write no code, no tests and no git
writes, and end the turn when the documents are in order. It must not call
`del_session` on itself: that waits on the gate its own turn holds, and the call
is abandoned at 120s.

## continue=false

`send_message` and the `handoff` skill both take `continue`, default `true`.
With `false`, the message is expanded and written into the session's context but
no turn is opened and a running turn is not interrupted; if the session is busy,
the row waits in the inbox as an inject. `handoff(continue=false)` still archives,
verifies and empties the transcript -- it only skips opening the continuation's
turn. The agent loop ends; the turn and its context remain.
