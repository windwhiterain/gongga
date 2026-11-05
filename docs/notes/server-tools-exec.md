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
