# `crates/server/src/tools/bash.rs`

## bash (module doc)

Keeping a single shell alive is what makes the harness feel like a terminal:
`cd`, exports and shell functions survive across tool calls, so the model does
not have to restate absolute paths forever.

Three ways to run a command, which the SPEC keeps separate:

* **foreground** (default): run it in the shell, stream the output, return the
  result. The turn waits.
* `background`: start it in the shell and return at once. Its output is tailed
  into the shell's *unread* buffer, which `check_bash` collects.
* `detached`: start a process of its own and return at once. It is not this
  shell's business -- see [`crate::tools::detached`], which owns it, and which
  is why a detached job outlives this shell, this session and even the server.

A marker makes completion detectable. After each command we print a sentinel
carrying the exit code to *both* stdout and stderr, so each stream can be
drained independently and we still learn the status.

## BashOutcome::still_running

The command had not finished when its deadline passed. It is *still running in
this shell* -- nothing was killed -- and the rest of its output can be
collected with [`BashShell::finish_outstanding`].

## BashShell::pending

Shared rather than owned so the tailing task can append without holding the
shell, and so a shell thrown away by `del_bash` does not strand the task
holding a dangling reference.

## BashShell::cwd_cell

Every path that updates `cwd` goes through [`BashShell::set_cwd`], so the cell
and the field cannot disagree. Outside this shell there is no lock-free way to
read anything about it -- a running command holds its mutex for as long as it
runs -- which makes the cell the only thing "where is this session" can ask
without waiting for that command.

## BashShell::set_cwd

The one way either copy of the directory is written, precisely so they cannot
drift apart. It exists for code that already holds the shell's lock, which is
the only code that knows where the shell actually is.

## BashShell::out_of_sync

One of the pipes ended without its sentinel, so this shell's output is no
longer aligned with its commands. Sticky: nothing can put the missing bytes
back, so the shell has to be replaced rather than reused.

## BashShell::job

Kept last on purpose: fields drop in declaration order, and the job's `Drop` is
what terminates the children still holding our pipes.

## BashShell::spawn

The extra variables are how the model finds its own server: `bash` is a child
of the server, so without them it has no way to know where to send a `gongga
exec` and has to guess or give up.

## BashShell::spawn (the process group)

Detach the shell into its own process group where the platform allows it, so a
timeout kill takes down the whole command tree.

## BashShell::spawn (joining the job)

Join the shell to a job before anything is written to its stdin, so every
command it runs is a member. On unix `process_group(0)` above does the same
job.

## BashShell::usable

False after a pipe ended without its sentinel: the shell holds a command
boundary it cannot find again, so a command sent to it would be answered with
somebody else's output. Whoever owns the shell has to replace it.

## BashShell::run

The deadline is a deadline on *waiting*, not on the command. When it passes the
shell is left exactly as it is -- the command keeps running, keeps its place in
the shell's own process tree, and its remaining output stays in the pipes --
and the outcome says `still_running`. The caller is then free to push this
shell aside and collect the rest later; killing it, which is what used to
happen here, threw away work that was simply long.

## BashShell::run (the bound timeout)

Bind the result rather than matching on the timeout directly: this ends the
statement that owns the borrow of `self`, so the branch below is free to mutate
the shell.

## BashShell::run (asking for `pwd`)

Asking where the shell is means writing one more command into the same pipe. A
shell that lost its place would answer that from the leftovers, so it is not
asked.

The same caller-supplied deadline bounds this internal command. If it expires,
the unread `pwd` sentinel is what makes this shell unsafe to reuse.

## BashShell::finish_outstanding

Only meaningful right after [`BashShell::run`] returned `still_running` on this
shell: the sentinel for that command is the next one the shell prints, so this
collects exactly its remaining output and exit status.

## BashShell::run_background

The shell redirects the job into a scratch file, which a task then tails into
the unread buffer; `check_bash` is how the model collects it. The job keeps the
shell's `cd`/exports, so a background command still runs where the foreground
ones run.

The wrapper writes the done sentinel, not the job, so three things have to hold
of the script:

* the job runs in an inner subshell -- `exit` only leaves the subshell and
  `exec` only replaces it, so control returns to the wrapper and the sentinel
  still gets written;
* the pid file lets the tailing reader finish when the wrapper itself is killed,
  since nothing is left to print the sentinel (`tail_background`'s pid check is
  the fallback);
* the sentinel is printed with `command printf`, which bypasses a user-defined
  shell function of that name -- the shell survives `cd`, exports and functions
  across calls, so a redefined `printf` would otherwise decide when the job
  looked finished.

## BashShell::run_background (the done marker)

`printf` takes the marker as an argument, never as a format string, so a
command that prints percent signs cannot corrupt the sentinel.

## BashShell::pump_both (a half-read pipe)

A pipe that ended without its sentinel has been left half-read: the rest of
this command's output is still in it, so the next command would read that
instead of its own. The shell is not repairable from here -- only replaceable
-- so it says so and the caller swaps it out.

## BashShell::drain (bytes, not `String`)

A command may print anything -- `cat` of a binary, a program writing in the
console code page -- and `read_line` into a `String` fails at the first byte
that is not UTF-8, discarding the rest of the line *and* leaving the pipe
half-read. The sentinel is then never consumed, and the shell answers every
later command by replaying the leftovers: in one session, after `cat $(which
gongga)`, five calls in a row returned nothing but `[read error: ...]` (`echo
hi` among them), and the call after that returned the *previous* command's
output while reporting success. Reading bytes and decoding lossily cannot
happen: the bad bytes become U+FFFD and the stream stays where it should be.

## BashShell::kill

The tree matters as much as the shell. A child of the shell inherits the write
end of the pipe we read from, and reading that pipe is something the runtime is
waiting on during shutdown -- so killing the leader alone leaves a reader
blocked on a close that only the children could have given it. The `wait` then
reaps the leader, which is what lets the reaping thread finish.

## BashShell::kill (why `out_of_sync` is set)

A killed shell is never handed another command. Whatever the pipes still hold
belongs to a command that is gone, so saying so here is what keeps a caller
that kept the handle from reading it later as if it were its own output.

## BashShell::kill (the tree, not the shell)

Children inherit the write end of the pipe this shell reads from, and reading
it is something the runtime is waiting on during shutdown -- so a surviving
grandchild means a reader blocked on a close that never comes.

## BashShell::kill (the fallback)

Fallback for the case the job could not be created: the leader still has to go,
and `taskkill /T` may catch some of its children.

## BashShell::cd

Kept as the primitive behind a `cd`-shaped tool; `bash` itself just sends `cd`
through, which is why this is currently unused.

## tail_background

Polling a file is not elegant, but the job lives inside a persistent shell we
cannot attach a pipe to without disturbing the foreground sentinel protocol, and
150ms is well below the granularity at which a model asks for output.

## tail_background (the notice)

A job that ends tells its session, which is what SPEC's "background bash ended
will notice its session" asks for. The count is decremented *before* the notice
is sent, so a reader woken by it never sees the job still listed as running.

`ended` is not "the tail stopped": a scratch file that never appears breaks the
loop too, and that is a shell problem rather than a finished job. Silence is the
honest answer there, because a status nobody measured is worse than no status.

## tail_background (the pid fallback is asked first)

A fast job exposed a race that predates the notice: the wrapper can exit between
one round's file read and the pid check, so the fallback used to conclude "ended"
while the sentinel and the last chunk of output were already sitting in a file
nobody re-read. The status then came back unknown and the output was lost -- the
quiet kind of loss, since a `background: true` call that prints one line looks
fine either way.

Hence the order: ask the pid first, read `offset..` second, and treat "the
wrapper is gone" as "this read saw the whole file". A gone pid now means the loop
had one last look rather than one look too few.

## wrapper_gone

A pid file that exists but does not parse is not evidence of anything: the shell
writes it after the wrapper starts, and an unreadable one must not be mistaken for
a finished job.

## BackgroundEnd, BackgroundNotifier

The shell knows about shells, not sessions: the notice is a closure the tools
layer supplies, so `bash.rs` stays free of the store and of the agent loop, and
`cargo test -p gongga-server tools::bash` can watch it fire without a server.

The exit status comes from the sentinel the wrapper writes; a job that died
without one (killed wrapper) reports `None` rather than a made-up zero.

## push_background_chunk (the code after the sentinel)

Settling keeps the bytes behind the marker instead of clearing the buffer: they
are the exit status. Dropping them would leave every notice saying the status
could not be read.

## cap_output

Caps tool output: the head and the tail survive, the middle is dropped and the
whole thing is written to disk so nothing is actually lost.

This exists because the transcript is replayed in full on every step. A 22 KB
`cat` is not a one-off cost of 22 KB -- it is 22 KB on every step after it,
forever, which is what drove a single turn to 17 MB of traffic. Capping is the
difference between "the model can see the file" and "the model can see the file
and every later request pays for it".

`dir` is where the full output is spilled; `label` names it. A cap of 0 means
no cap, which is what a deliberately unbounded run wants.

## cap_output (head and tail)

Slightly more head than tail: the start of an output usually says what the
command was about, the end says how it ended.

## spill

Writes the full output under `dir`, returning the path in Git Bash dialect so
the model can feed it straight back to `sed` or `grep`.

## slug

ASCII alphanumerics survive, and so does every non-ASCII alphanumeric: a model
writes its `task` in whichever language it is thinking in, and replacing `中文`
with `---` would throw away the only part of the name a human could have read.
Everything else -- spaces, slashes, colons -- becomes `-`, because those are
what make a path either unreadable or plain wrong.

Bounded because this ends up in a *path*, not because the work is bounded: the
label already carries a session name and a call id, and Windows refuses
components over 255 characters. A shorter slug is not a smaller budget.

## slug (`is_alphanumeric`)

`is_alphanumeric` is Unicode-aware, which is the whole point: it keeps the CJK
and accented text a task is likely written in, while still rejecting `/`, `:`
and spaces.

## tests::a_hostile_task_cannot_escape_the_output_directory

Every label ends up in a path, so the one thing this must never do is produce a
name with a separator in it -- whatever the model typed.

## tests::a_finished_background_job_tells_its_session

Two jobs, one succeeding and one failing, because a notice that always says zero
is the failure worth catching: the status has to come from the sentinel of the
job it belongs to, not from whatever the shell printed last.

## tests::background_output_waits_for_check_bash

The one thing about background jobs that cannot be tested by reasoning: the
scratch file has to be writable *and* findable from both sides.

## tests::binary_output_does_not_deafen_the_shell

A command that prints bytes (`cat` of a binary, a program writing in the
console code page) puts something into the pipe that is not UTF-8. The reader
used to stop at the first such line with `[read error: ...]`, leaving the rest
of the output -- including the sentinel -- unread in the pipe. Every later
command in that shell then read the leftovers, failed on the same byte, and
reported success with the same one-line error: a shell that looks alive and
answers nothing. This is what one session showed after `cat $(which gongga)`:
five calls in a row, `echo hi` among them, all returning the read error and
nothing else.

## tests::a_shell_that_ended_is_not_reusable

`exit`, a kill, or a crashed child closes the pipe without a sentinel. There is
no way to resynchronise from that, so the shell reports itself unusable and is
replaced, instead of answering the next call with whatever was left in the
pipe.

## tests::a_killed_shell_reports_itself_unusable

The cancel path stops the command by killing the shell's process tree, and then
replaces the shell under the same name. Anything still holding the old handle
must not read what is left in its pipes as the next command's output, which is
what `usable()` gates.

## tests::spawn_hands_the_shell_its_variables

The model drives the server through the CLI, which means the shell has to know
where the server is. Nothing else in the process tree tells it.
