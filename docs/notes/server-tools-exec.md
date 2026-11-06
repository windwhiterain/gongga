# `crates/server/src/tools/exec.rs`

## `exec`: one program, its arguments, no shell

`bash` runs a command line, and a line is read by a shell: the words in it are
split, expanded and globbed, and the quoting that says which is which is the
model's to write. That is the CWE-78 shape -- data and code in one string --
and it is not a shape to make safer by quoting more carefully. The fix is not
to quote better; it is to stop mixing.

`exec` takes the program and its arguments as a list of words and spawns the
program with them. No shell reads them. So there is nothing to escape: a word
holding a space, a quote, a `;`, a `$` or a `*` reaches the program as itself,
because nothing between the words and the program ever looks at them.

Measured on the real tool, with a native program that dumps its own argv:

```
'a b' | "it's" | 'a;rm -rf /' | '$HOME' | '*' | '`id`' | 'a|b' | 'say "hi"' | 'C:\dir\' | 'ünïcödé'
```

Every word arrived exactly as it was written. The same words through `bash`,
where the model has to quote them itself, reach the program the same way only
when every quote is right -- and one wrong quote is not a wrong argument, it is
a broken command line that hangs the shell.

## What `exec` gives up

No shell means no shell. `exec` cannot pipe, redirect, glob, expand a
variable, chain with `&&`, or `cd` -- and it is not the persistent shell, so
`cd` and exports do not survive the call. A program that is a shell builtin
rather than a program (`cd`, `export`) is not a program and cannot be run.

That is the trade: `bash` is for what a shell does, `exec` is for running a
program with arguments that are data.

## A program that re-reads its own command line

`exec` does not interpret anything, but a program may interpret its own
arguments. Git's `usr/bin/printf.exe` is an MSYS program: it takes the Windows
command line and re-parses it with shell rules, so `it's` arrives as `its` and
`*` arrives expanded. A native program (`python.exe`, anything not built
against the MSYS runtime) gets its arguments as they were written.

The lesson for a caller: `exec` delivers the words; what a program does with
them afterwards is the program's own business.

## The NUL byte

Refused. An argument ends at a NUL for every program there is, so a word
holding one can only arrive as a different word -- silently. The call is
refused instead, and the refusal says why.

## A wait that is cut

A message, or a cancelled turn, ends the wait in `ToolRegistry::run` for every
tool alike. What happens to the program is the stop's answer, not the tool's
choice:

* a `cancel` a client asked for lets jobs go, so the program is left running -- it
  was started as a process of its own, and the row it was recorded as is the next
  start's to adopt;
* every other cut ends it with the call, and the answer says what is unknown: what
  it printed after that point, and its exit status.

A peer's program is the exception in the first case: `remote_exec` has no local
process to let go, so it ends with the call. `commands::cancel_lets_go` is where
the two stops are told apart, set before `cancel` interrupts anything.

`bash` has a shell to leave the command in; `exec` has a process, which is the
same promise by another name.

## The clock's end: a program that is still running

A foreground program is started the way a job is: a group of its own, armed to the
server, both streams into one log. The row is written while the call waits, and
what the wait decides is only who reads it:

* it answers in time: the row is deleted, its output is the answer, and nothing is
  left for `check_job`;
* the clock runs out: the row stays, the process stays, and the call answers with
  the job id. `check_job` collects what it printed, and a server that stops before
  it ends is one whose next start adopts the row -- and reads the log as the answer
  if the answer file was never written.

That is SPEC's "foreground job running over timeout become background": the wait
ends, the work does not. `OwnsItsWait` is how `exec` keeps the tunnel's clock from
firing at the same instant and recording a second job for one call.

The reading tools (`read_file`, `write_file`, `replace`) and `skill` still promote
into a job of this server rather than a process: a worker per file read costs more
than the read, and what they wait for is a file, not a build.

