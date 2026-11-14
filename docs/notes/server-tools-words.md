# `crates/server/src/tools/words.rs`

## module doc

`exec` had the spawn a skill's entry needed: the program and its words, no shell between
them, both streams read while the program runs, a deadline that stops it rather than leaving
it behind, and the error wording a reader acts on when a name is not on PATH. Copying that
into a second tool would have been two places for a pipe to deadlock.

What moved is only the part that is the same for both: `run(program, args, stdin, cwd, env, deadline)`.
What stayed in `exec` is what makes it `exec` -- the word list's validation (a word holding a
NUL is refused, because an argument ends at one, and the program may or may not be on the
machine), the machine choice, and how the run is reported.

## The request a program reads

`stdin` is the call itself, for the one caller that has a call to hand over: a package
skill's contract reads one JSON line and answers on stdout. It is written from its own task
while the program runs, so a request larger than a pipe's buffer cannot deadlock against a
child that is still starting, and dropping the pipe is the EOF that tells the contract the
line is complete. `None` is `/dev/null`: a program that reads stdin anyway sees the end of
it, rather than whatever the server has on its own.

## The directory a program stands in

`run` is handed a directory and always starts the program in one. What it is handed may be
a directory this machine does not have, and the rule is the same everywhere in this layer:

| who asked | a directory that does not exist |
| --- | --- |
| the call itself (`exec`'s `cwd`) | refused by its caller, naming that path |
| the session, as a default | refused by its caller, naming it as the session's |
| a session directory used directly (a skill's entry, `new_channel`, a file tool) | never an error: `tools::standing_dir` answers this server's own directory |

The third row is the one measured: a session whose directory is on a peer has an empty
one here (`Workspace::dir` is "empty on the other machine"), and `Command::current_dir` of
an empty path fails on Windows with `ERROR_INVALID_NAME` -- which a spawn error used to
report as `could not run "...bash.exe": ... (os error 123)`, sending the reader after a
broken interpreter instead of a missing directory. `scripts/spawn_dir_selftest.sh` is the
real-process check.

## Why no shell

The program is spawned with `Command::new(program).args(words)`. On Unix that is `execve` and
its argv array: the words are the words, with no string in between. On Windows
`CreateProcessW` takes one command line, so a string exists -- but it is *generated* by the
runtime from each word (quoting, escaping backslashes the way `CommandLineToArgvW` reads them
back), not *parsed* from text somebody wrote. Nothing here re-reads it as a language: no word
splitting, no `$` expansion, no globbing, no redirects, no `&&`.

Measured on this machine, on the real path, with a program that dumps its own argv: a word
holding a space, a double quote, `$HOME`, a `;`, a `*`, a trailing backslash, `%PATH%`, an
ampersand, a pipe, a tab or a `CJK 空格` arrives exactly as written. That is what the two
tools promise, and it is the reason neither takes a command line.

The one thing this does not cover is the *program* being a shell: `["bash", "-c", "…"]` starts
a shell with a string, and then the string is code again. That is the caller's word to give,
not this module's to refuse -- but it is why a skill is started as an entry and an argv instead of
letting a body's instructions reach a shell.

## The environment the child gets

`run` takes the pairs to set, and its two callers pass `ToolContext::env` -- the session's
own facts, not this server's inherited environment, which is whoever launched the server.
See `server-session.md`, "session_vars and SessionHandle::env": a program that answers
`$GONGGA_SESSION` with somebody else's name is not a convenience missed but a wrong answer,
and `subagent`'s `spawn` decides a child's parent from exactly that variable.
