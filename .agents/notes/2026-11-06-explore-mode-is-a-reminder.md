# Explore mode is a reminder, not a jail

Decided with the user on 2026-11-06. **No code yet.** This note is the record of what was
decided and of how much of it the server already gives; the contract moves into
`docs/notes/server-commands.md` and SPEC when the first line of it exists.

## What the user asked for

Three turns, each cutting something away:

1. 「在 agent 可以用 bash/python/exec 的情况下，如何设计 explore mode 让 agent 只读？」
2. 「不需要完全防御，只要 agent 忘记只读时被提醒一下。」 -- a nudge, not a gate.
3. 「在 bash/python/exec spawn 的进程结束时做一次这个检查，只读提示和 notice 一起送给
   agent 行不行？」 -- and 好 on the split: the foreground answer rides its own result, the
   asynchronous one rides a notice.

Two corrections the user made to the first draft stand:

- Tool execution and the model stream are **not** concurrent for a foreground call:
  `agent.rs:1054` runs the call inside the stream's `for await`, and
  `run_before_cache_expiry` (`agent.rs:655`) exists to bound how long a tool may hold the
  prefix cache. A check awaited in the tool path is therefore not free -- it is paid by the
  step that can least afford it.
- But only the *spawn* is synchronous: every tool takes `background` / `detached`, and those
  run on while the stream continues. So a check keyed on a **job's end** is off the awaited
  path, while a check keyed on the **step** misattributes a background job's write to a
  later step, or misses it entirely when the job outlives the turn.

## Why not pattern-match the command

A gate wants recall; a nudge wants precision, and **a reminder the agent has learned to
ignore does not exist**. Regexes over a command line are the weakest precision layer
available: `git log 2>/dev/null | head`, `grep -rn "rm -rf" docs/`, `echo "…rm…" >> notes.md`
are everyday false positives, and the misses land exactly where the writes are -- `python -
<<EOF` bodies, `bash scripts/x.sh`, `cargo metadata` writing `Cargo.lock`, `git status`
refreshing `.git/index`, `ln -s`. Intent matching survives only as the last layer, over
tokenised words and known subcommands, for what nothing else can see.

## The seam: job end, not process end

`python{code}` is a persistent REPL and `bash{name}` a persistent shell: neither process ends
per call, so "the process the call spawned exits" names no event for either. The event that
does exist is the one the harness already has -- a job's end, `tunnel::report`
(`tools/tunnel.rs:436`), reached from the ordinary spawn (`:243`), the clock watcher that
promoted a foreground call (`:538`), and the detached path including a job adopted after a
restart (`:425`). One hook there covers all three tools in every mode, and it is already
`async` and already `tokio::spawn`ed (`lib.rs:131` is a multi-thread runtime).

Foreground calls never reach `report`: they run inline and return a `ToolOutcome`. That is
why the reminder has two carriers and not one -- and it is what makes the split cost-free
rather than a preference:

| the call | when the agent learns | carrier |
| --- | --- | --- |
| foreground | with the answer it is already reading | a trailer on that call's result |
| `background` / `detached` | when the job ends | a notice in the inbox |

## Delivery, and why it costs no turn

A notice is queued by `store::finish_job` (`store.rs:2466`) as a `pending_tasks` row of kind
`inject`, and `commands.rs:2222` already writes down the invariant this design needs:

> A notice about a background job is news for a working session; on its own it must not cost
> an idle session a turn.

`owes_a_turn` (`commands.rs:2232`) answers false for a notice alone, `claim_recovery`
(`commands.rs:223`) returns `None`, and no turn is started. The row waits in sqlite
(`store::deliver_pending`, `store.rs:2147`) and is folded into the transcript at the next
step boundary -- immediately after a foreground call, whenever a background job ends, and
otherwise at the session's next real turn. A restart loses nothing, which is more than a
`tokio` task could say.

Two consequences to honour:

- **Use a new kind** (`"readonly"`), not `"background"`. `resume_pending` (`commands.rs:2356`)
  deletes a `"background"` notice on restart, because what it points at -- a shell's unread
  buffer -- died with the process. `"job"` is deleted too, its `tool_jobs` row speaks for
  itself through `announce_owed`. Anything else falls through to `_ => kept`, which is what a
  self-contained reminder wants.
- **The kind is a notice, the text is a message.** `payload["notice"]` decides only whether
  the row owes a turn; `deliver_pending` still turns the payload into a transcript row the
  next request is built from. A reminder that no request sees would not be a reminder.

## Cost

Measured on this machine (Git Bash / MSYS, 370 tracked files, 8 dirty):

| probe | time |
| --- | --- |
| full-tree mtime+size fingerprint | 76 121 ms -- unusable (`.dev` 3.3 GB, `target/`) |
| `git --version` (spawn floor, `GIT_OPTIONAL_LOCKS=0`) | 76-121 ms |
| `git status --porcelain` | 83-136 ms over 8 runs |
| `git status --porcelain -uno` | 72-139 ms over 5 runs |
| `git ls-files` | 89 ms |

The scan is free; **the process spawn is the entire cost**. So: one spawn per job end,
`tokio::spawn`ed, `GIT_OPTIONAL_LOCKS=0` (otherwise the detector itself writes `.git/index`
and contaminates the next comparison), and no full-tree walk ever -- a cwd outside a
repository has no detector rather than a slow one. Added latency on the awaited path: 0.

## What this does not give

- **The first write still happens.** A reminder names the path after the fact; it stops the
  second write, not the first. So the identity layer stays: `write_file`, `replace`,
  `subscribe --cmd` and `detached` are refused outright in this mode, because their identity
  *is* the write and there is nothing to classify. A gate placed in `ToolRegistry::run`
  (`tools/mod.rs:552`) runs before any carrier resolves, so a refused call spawns nothing.
- **A long job's reminder is late** -- minutes, for a `cargo build`. The intent layer is what
  compensates: outside the workspace, network writes (`git push`, `curl -X POST`), delete and
  move out of the workspace, matched on tokenised words and known subcommands, failing open.
- **Attribution degrades under concurrency.** Two jobs ending together may both report the
  same changed paths. Chained snapshots would need a per-session lock; a reminder does not
  need blame, so it names "what changed while your job ran".
- **Nothing outside the workspace, nothing ignored, nothing on the network.** `target/`,
  `.dev/`, `/tmp`, a push -- all invisible to a workspace diff. Stated in the note and in the
  reminder's own words, not left to be discovered.
- **The name is not `read-only`.** A mode that cannot promise writes are impossible must not
  be called by the word that promises it.

The python audit hook (`sys.addaudithook`: `open` with a write mode, `os.remove`,
`os.rename`, `os.mkdir`, `subprocess.Popen`) and `trap … DEBUG` with `$BASH_COMMAND` are the
exact versions of this signal, at zero per-call cost, and are worth a second look if the diff
proves too late or too coarse.

## Built

The plan above changed in three places while it was built, and the code is the record:
`docs/notes/server-explore.md` says what exists.

- One delivery path, not two: every reminder is a `pending_tasks` row with `notice`, so a
  foreground call needs no trailer either -- the step boundary delivers it as a message,
  and a row that owes no turn waits for the next real one.
- One field for two facts: `ToolContext::explore: Option<Arc<Watch>>`, whose `None` *is*
  work mode. Carrying the mode and the snapshot separately would let the two disagree.
- `detached` is not refused. The job-end look covers a detached call and a job adopted
  after a restart, so refusing it would be a guarantee the reminder does not need.

`subscribe --bash --cmd` and `subscribe --python --code` are still uncovered by both
layers; the OS-level jail (`@anthropic-ai/sandbox-runtime` for a real guarantee) remains
the answer if the user ever asks for enforcement rather than a reminder. SPEC has no
section yet: it is the user's file, and this ships under `set_session --mode`.
