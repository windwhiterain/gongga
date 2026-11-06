# `crates/server/src/explore.rs`

## explore -- a mode that reminds, and says so

`--mode explore` on `set_session` asks a session not to change the workspace. It is
called a mode and not a sandbox on purpose: a name is a promise, and this one cannot
promise that nothing is written. What it promises is that a session cannot write by the
two tools whose arguments *are* the write, and that a program which wrote anyway hears
about it before it answers.

The distinction is the whole design. `bash`, `exec` and `python` take text, and whether
that text writes is a question about behaviour: `git status` writes `.git/index`,
`cargo metadata` writes `Cargo.lock`, `python - <<EOF` hides anything at all, and
`grep -rn "rm -rf" docs/` writes nothing while reading like it might. A guess about the
text is therefore wrong in both directions, and the wrong-in-the-useful-direction
result -- refusing `git log 2>/dev/null | head` -- is what teaches an agent to ignore
the mode. The two questions are asked where an answer exists instead:

- *can this be refused exactly?* The identity layer: `Tool::writes` is true only where
  the arguments are a path and its new contents. There is no text to classify, so the
  refusal cannot be wrong.
- *what did the workspace do?* The effect layer: after a program call ends, one
  `git status --porcelain` says which paths differ from the last look. A program cannot
  write without the filesystem recording it, so this needs no opinion about the command.

## Mode

Two words, `work` and `explore`, and `work` is what an absent word means. A value this
build cannot read is `work` rather than an error: a session is judged by the row that
holds it, and a row written by a newer build must not make an older one refuse to
serve it. `Mode::parse` is the only place a word becomes a mode, so the compiler is
asked for the refusals rather than a list being kept beside them.

## Watch

One snapshot per session, kept here rather than in the store: a snapshot is not state a
restart must restore. Missing a change across a restart means one reminder not sent, and
a restarted server has no session that was mid-explore with a memory worth keeping --
the next look starts the chain again.

The probe is `git`, not a walk of the tree. Walking the workspace measured 76 seconds on
this repository (a `.dev` directory of 3.3 GB and a build `target/`), while
`git status --porcelain` measured 83-136 ms and `git --version` alone 76-121 ms -- so the
list of tracked paths is nearly free and the process is the entire cost. A directory
that is not a repository has no probe: the exit status says so, the look is `Blind`, and
the chain ends. It never falls back to the walk.

`GIT_OPTIONAL_LOCKS=0` is not tuning. `git status` takes the index lock unless it is
told not to, which refreshes `.git/index` as a side effect of reading -- the next look
would then compare the workspace with the record of its own last probe and call the
index a change.

The state is a mutex, and a poisoned one is `Blind`. A lock that was poisoned means a
look panicked holding it, which must not become a second panic on every later call: the
mode's honesty is worth more than its precision.

## Watch::look

The comparison is between two sets of status lines, so it reports a path that entered
the set and a path that left it. `First` and `Blind` are not `Same`: a look with nothing
to compare against must not read as "nothing happened".

## explore::prime

The chain has one end that must not be left loose. `First` is silent, so the first change
after the mode went on would *be* the baseline and would never be reported -- and that
first change is the common case, not the corner: an operator switches a session to
explore and the agent's next `bash` writes. So the baseline is taken when the mode goes
on (`set_session`) and when a session that was reading materialises after a restart.
Both take it on a task of their own and drop the answer: a primer that reported would
report the whole tree it had just met.

## is_program

The tools whose result is a program's: `bash`, `exec`, `python`, `skill` and the
`remote_*` family. A file tool's writes are its identity and are refused, so looking
after one would be looking twice. `read_file` is not in the list because it cannot
write.

## after_job

Called where a call ends, which is two places and not one: a foreground call ends in
`ToolRegistry::run` -- the one funnel of a model's call, a command layer's and a
subscription's alike -- and an asynchronous one (`background`, `detached`, promoted by
its clock, or adopted after a restart) ends in `tunnel::report`. Both are job ends, so
both look.

The foreground half sits in `run` around `invoke` rather than in the turn, because the
turn is not the only caller: an operator's `bash <session> ...` and a subscription's
`--bash` action reach the same `run`, and the workspace they touch is the session's.
Placing the look once, there, is also why a refused call looks at nothing -- it returns
before the look, having spawned nothing to look after.

The look runs on a task of its own. The alternative is for the step to wait for a
process, and a step already waits for the tool: the harness runs a tool call inside the
stream's event loop, and `run_before_cache_expiry` exists because that wait can let the
prefix cache expire. A 100 ms process added to every call would be paid from the budget
that function protects, for a reminder the model reads a moment later either way.

`remote.is_some()` is skipped and this is a real hole: a peer's workspace is not on this
filesystem, so a remote call's writes are neither refused nor reported. `docs/notes/`
until the mode is carried across the link, the honest statement is that explore mode is
about this machine.

## the pending task and its kind

The reminder is a `pending_tasks` row of kind `inject` carrying `notice: "explore"`.
That key is what makes it a *notice*: `owes_a_turn` is false for it, so a row that
arrives when the session is idle waits for the next real turn instead of spending one,
and `claim_recovery` has nothing to reclaim. The text is still a message: the step
boundary delivers it into the transcript, and the next request is built from it.

The kind is not `background`. `resume_pending` rewrites a `background` notice into a
real message that owes a turn, because a job the stopped server never reported is news;
an `explore` row that a restart replays is a reminder about a workspace the restart did
not touch, and it keeps its own kind so the pass leaves it alone.

## What is not covered

Said once here so that nothing else has to repeat it: `subscribe --bash --cmd` and
`subscribe --python --code` run on an event, not as a tool, so neither layer sees them;
anything written outside the session's `cwd`, to an ignored path, or into the network
(`git push`, `curl -X POST`) leaves no status line; and the mode does not cover a second
server sharing the same directory. The refusal is exact and the reminder is late -- it
arrives after the write, which is why its text says what was changed, that nothing was
undone, and how to put it back.
