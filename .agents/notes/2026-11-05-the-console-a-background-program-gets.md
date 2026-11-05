# The console a background program gets

## The problem

A Windows program with no console gives every console program it starts a new
**visible** console: a window appears on the desktop, titled with the child's
path. The harness starts programs in the background, and each one put a window
there.

## The fix

`crates/console` holds `CREATE_NO_WINDOW` and the rule that adds it to the flags
a caller already needs -- the standard library replaces the creation flags
rather than adding to them, so the two have to be computed together.

The flag goes where the parent has no console to hand down:

- `server::supervise` -- the supervisor's restarted server;
- `server::remote::runner` -- the shell on a peer, next to `CREATE_SUSPENDED`;
- `cli::skill_command` -- the Python probe;
- `bundle::payload_impl` -- the `cancel` an upgrade sends.

It does **not** go where the parent already holds a console: the server's own
children inherit its invisible console, and the TUI's children inherit the
reader's terminal. `bundle::take_over` stays attached for the same reason.

## Measured

| what was started (parent without a console) | before | after |
| --- | --- | --- |
| the server, restarted by the supervisor | one window, titled with the exe's path | none |
| the command layer's Python probe | one window, titled `AppInstallerPythonRedirector.exe` | none |

## Left over, on purpose

Running a skill's entry still shows a window when the command layer itself has
no console: the entry is meant to inherit the reader's terminal, and a flag
would give it an invisible console of its own.

## Where

- code: `crates/console/src/lib.rs`
- reasoning: `docs/notes/console-lib.md`
- branch: `fix/no-console-window`, merged into `main` as `f82052f` and `678778c`
