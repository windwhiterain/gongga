# 2026-11-10 — SPEC's argument names, and a path that takes what is under it

The operator said "改" after the flag work, with one clarification: *"删除 path 指的是
删除这个路径下所有的实体."* So this pass is the cross-cutting rename that had been
sitting as Q1/Q2 since 2026-11-08, and the subtree rule that comes with `path`.

## What the catalog says now

| role | word | verbs |
| --- | --- | --- |
| the session a command acts within | `session` | `send_message`, `get_messages`, `ls_channel`, `del_channel`, `rename_channel`, `new_channel`, `input_channel`, `ls_subscription`, `del_subscription`, `subscribe` |
| the one entity addressed | `id` | `set_session`, `new_session`, `interrupt_session`, `clone_session`, `del_channel`, `rename_channel`, `new_channel`, `input_channel`, `subscribe` |
| a place in a namespace | `path` | `ls_session`, `del_session`, `ls_channel`, `ls_subscription`, `del_subscription` |

`clone_session(id, new_id)`, `SessionConfig.keep_alive` (was `--cache_keepalive`), and
`subscribe --id` complete it. `subscribe`'s name stays named-only: an optional first
positional cannot be told apart from the first required one.

## The subtree rule

`commands::under(name, path)` — a component match, not a string prefix (`a/b` is under
`a`, `ab` is not; empty is everything) — is now what every `path`-taking command uses,
for listing *and* for deleting. `del_session main` deletes `main` and `main/a`; a
`del_channel c/x` ends `c/x/1` and `c/x/2`; `del_subscription hook` drops `hook/one`
and `hook/two` and leaves `other`. Sessions are deleted one at a time so each keeps its
own cleanup (shells, jobs, channels, subscriptions).

## Measured

One server, one home: `set_session --id a`, `--id a/b`, `--id a/b/c`, `--id ab`;
`ls_session --path a` answered `a/b, a/b/c, a` (no `ab`); `del_session --path a`
answered `deleted 3 session(s) under "a"`; `ls_session` then showed only `ab`.
`interrupt_session --id a/b` and `send_message --session a/b` answered. With three
subscriptions `hook/one`, `hook/two`, `other`, `del_subscription --path hook` dropped
exactly two. `channel_selftest` PASS, `tool_selftest` PASS, `drain_selftest` PASS;
`cmd_selftest` 3 FAILs, the same three tool-command checks that fail on `fc44148`
(the forwarding migration's open end), and its `delete_session` operation checks now
pass because `Operation::DeleteSession` builds `path`, not `name`.

## The content had to move with it

`ship/` spells these flags in 8 files (skills' python and their SKILL.md prose), so the
pair was cut over in the documented order: content first. `ship/ship.toml` is protocol
9 with the reason written next to it, `scripts/ship.sh -m "protocol 9: ..."` committed
`ebde89a` on `local`, rebased and pushed to `origin/main`; then `crates/server/ship.toml`
came to 9 and `scripts/ship.sh --check` says "in agreement". The skills' own tests pass
(`test_new_session.py`, `test_heir.py`), and `new_session.py`'s `--name` for the source
session became `--id` while the *column* it copies stays `cache_keepalive`.

## Left alone, deliberately

- `ls_skill` / `run_skill` / `get_messages` / `clone_session` as a shipped uv package
  (SPEC's `## Python Cmd`): still not started; `skill` is a Rust tool today.
- The tool-forwarded commands (`new`, `check`, `wait`, `bash`, ...) still reach the
  server through `tool_cmd`, and their arguments are the tools' (`--path`, `--name`,
  `--args`). SPEC gives the tools a different vocabulary on purpose (its own Q7 note);
  collapsing the eighteen is the separate deletion.
