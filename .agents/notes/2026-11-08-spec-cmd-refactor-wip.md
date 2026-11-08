# 2026-11-08 — advancing on SPEC's `## Server Cmd` while the operator is away

The operator left with "按照 SPEC 的精神自助推进"; these are the points I would have
asked about, and what I did instead. Read the last section first if you only want the
open questions.

## Landed

1. `wait`'s clock is the argument `time` (SPEC: `fn wait(time: time)`), and the rename
   moved content to protocol 6: a tool argument is a shape a server reads out of the
   call, so protocol 5's tip keeps `timeout_ms` and an older build walks back to it.
2. `subscribe` carries a command, not a shell line and not a channel write:
   `subscribe <pattern> <cmd> --session <session> --persistant`. The row stores
   `{"name":…,"args":{…}}`, a match runs it through `commands::layer()` — the same door
   a tool's forwarding uses — and the outcome is the `subscription` log row.
3. `new_channel`, `input_channel`, `rename_channel`, `ls_channel`, `del_channel` are
   commands now (they were `ls_job`/`del_job`/`rename_job`; the first two are new).
4. `ls_session`, `ls_subscription`, `del_subscription` (was `del_subscribe`).
5. `new_session` exists; `set_session` keeps its `--must_create` so today's skills and
   scripts keep working.

Gates run after each: `cargo build`, `cargo fmt --check`, `comments-check`,
`channel_selftest.sh`, `tool_selftest.sh`, `cmd_selftest.sh`.

## Decisions I made without asking

**`subscribe`'s name is named-only (`--subscription`).** SPEC writes
`subscribe(id: option[Subscription] = none, pattern, cmd, session)`. An optional *first*
positional cannot be told apart from the first required one on a command line: with
`subscribe <p> <cmd>` the reader cannot know whether `<p>` is the name or the pattern. So
the name is `--subscription` and the three required arguments keep their order. A call
that names no subscription is given the first free `sub<n>`.

**`input_channel` takes `wait` and `until` as well.** SPEC's signature is three
arguments, but a channel's reply has to be bounded somehow and the marker is how a
caller frames one. Without them the command could only ever wait its default. Both are
optional.

**`new_session` is `set_session` with `--must_create true`.** Creation and re-tuning are
the same write; SPEC splits the verbs, so the split is a refusal, not a second code
path.

## Open questions (no answer invented)

**Q1 — `session`/`id` argument spellings.** SPEC spells every session argument
`session: Session` and every addressed entity `id`. Today `send_message`, `set_session`,
`del_session`, `interrupt_session`, `get_messages`, `clone_session` take `--name`, and
the channel commands take `--path`. Renaming is a cross-cutting CLI change: it must ship
with a content republish, because `self-control`'s `new_session.py`, `subagent`,
`polity` and a dozen selftests spell `--name` today. One sweep, or leave it?

**Q2 — `keep_alive` (SPEC) against `--cache_keepalive` (today).** Same shape of problem:
the argument is spelled `cache_keepalive` and `self-control/new_session.py` passes it. The
machinery (a session column and the tuning path) exists; only the name differs. Bundle
with Q1?

**Q3 — should `--must_create` leave `set_session`?** SPEC drops it (`new_session` is the
verb that refuses). Dropping it breaks every caller that guards a name today. I kept it;
say when.

**Q4 — SPEC's `SessionConfig` is nested (`model_config`, `workspace`).** I kept flat named
arguments (`--model`, `--thinking`, `--ctx_limit`, `--cache_tier`, `--remote`, `--cwd`).
The nesting is a *value* shape: is a command line supposed to spell the tables, e.g.
`--model_config '{"model": "x/y"}'`, or is the flat spelling the CLI's own?

**Q5 — `fn <tool>(**args)` forwards tool → cmd; today the dispatch maps cmd → tool.** In
`commands.rs` a name that is not a command falls through to
`tool_cmd::dispatch_tool`, i.e. the *command* surface reaches the *tool* registry. SPEC
inverts it: `new`/`check`/`wait` are the only tools with a meaning of their own, and
every other tool is a command. Doing that collapses the tool table into the catalog —
which is the "eighteen go" plus the `[tool.*]` rows — and it changes which door refuses
an unknown name. It is the biggest remaining slice and I did not start it.

**Q6 — the `## Python Cmd` section is not started.** `ls_skill`, `run_skill`,
`get_messages`, `clone_session` are named as a shipped uv package there; today `skill`
is a Rust tool, `get_messages`/`clone_session` are Rust commands and `skill run` goes
through `ServerScript`. Moving them is a package-shaped change (a wheel, discovery, the
`ServerScript` door) and wants its own session.

**Q7 — `show`/`input_channel` naming.** SPEC splits "start a channel" (`new_channel:
id, name, session`) from "write into one" (`input_channel: id, input, session`). The
`new`/`check` tools keep their own names and their own framing (`args` for the program,
`input` for a write). Two vocabularies for one thing, by SPEC's own design: the tools
speak a model's language, the commands an operator's. Just recording it.

## Still owed from before

`docs/notes/server-channel.md` § "Still owed: the eighteen go". Its slice 1 ("names that
mean two things") is now done by the rename: no command shares a name with a tool. The
deletion itself (about 5200 lines) is untouched, and the tools still reachable are
`bash`, `python`, `exec`, `remote_*`, `check_*`, `del_*`, `list_*`, `detached`.
