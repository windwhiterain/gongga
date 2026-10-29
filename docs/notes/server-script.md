# server/script.rs

## script

Commands registered by a Python script, with no Rust change. The server scans
`<home>/scripts/*.py`, asks each one what it declares, and publishes what comes
back in the same greeting as its own commands. From then on a call is one
subprocess: one JSON request on stdin, one JSON reply on stdout.

The point is not "plugins". It is that the catalog has one owner. A front-end
that kept its own list could not gain a command without a rebuild; here a script
that is added, edited or removed changes what the next greeting says, and every
client follows -- completion, `--help`, validation and all.

## The request and the reply

    {"op": "manifest"}                        -> {"commands": [...]}
    {"op": "run", "command": "x", "args": {...}, "home": ..., "server": ...}
                                              -> {"ok": true, "message": ..., "data": ...}

A declared command carries the same fields the built-in ones carry: `name`,
`description`, `args`, and per argument `name`, `description`, `required`, `value_hint`
and `completions` (`sessions`, `paths`, or omitted). That is deliberate: a
script command is validated and completed by exactly the code a built-in is, so
there is no second, weaker path.

`ok: false` is a refusal the caller reads as a failed command, not as a crash.
The last non-empty line of stdout is the reply, so a script may log above it;
stderr is where a diagnosis comes from when the exit status is not success.

The server passes `GONGGA_CLI`, `GONGGA_SERVER` and `GONGGA_HOME`, so a script
can drive the harness through the same door the model uses. The example in
`scripts/server_script_example.py` does exactly that.

## Re-reading

`commands()` compares a fingerprint (path, size, mtime) of the directory and
re-runs the manifests only when it changed. The scan happens when a greeting is
built, so adding a script takes effect at the next client connect -- no restart,
and no subprocess per command.

## A broken script is a problem, not a failure

A script that prints no manifest, hangs, or prints something that is not JSON is
skipped, and the reason is kept in `problems()`: logged at startup, returned
when something asks for a command that no script registered. The rest of the
catalog stays. One bad file must not cost the operator every command they have.

Discovery has a deadline, and it is a hang guard, not a budget: a manifest that
never arrives would otherwise wedge startup, and there is no `cancel` to save a
server that never got as far as listening. Running a command has no such
deadline -- that work is the caller's, and only the stop ends it.

## stop_all

A script is a child process. `cancel` must not leave one behind any more than it
may leave a turn behind, so every running pid is tracked and killed when the
server stops. The caller waiting on that command is told the server stopped,
rather than being shown the killed process's exit status as if the script had
failed on its own.
