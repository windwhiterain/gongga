# server-supervise: the process that keeps the server running

SPEC: "server is wrappered by a watchdog process that auto restart if it
crashed". The wrapper is `gongga-server` itself, started without
`GONGGA_SUPERVISED` in its environment:

```
gongga-server …            <- supervisor: no runtime, no lock, no protocol
  └─ gongga-server …       <- the child: GONGGA_SUPERVISED=1, the server
```

## Why the server binary, and not a new crate

Three entries run a server, and all three run the *binary*: `cli::server::start`
for a window and for a one-shot command, the bundle's payload, and -- the one that
decided this -- `evolve`'s successor, which the launcher starts by copying the old
server's argv and `exec`ing the new file with it. A wrapper that was a fourth
program, or a mode of the command layer, would be absent exactly where it is
needed most: after a handover.

So "supervised" is a property of the binary, and the switch is an environment
variable rather than a flag because `evolve` copies argv and not the environment.
A flag would be copied into the successor's argv, and the successor would be its
own supervisor: unsupervised, with an argument in the log saying it was.

## The child gets this argv, `--parent` included

`--parent <window pid>` is passed through unchanged, so the stop the server
already knows still works. When the window dies, `signal::follows` asks for the
stop, the turn is preserved as a pending row, the server exits 0, and the
supervisor sees a stop rather than a crash. That is the whole of the
supervisor's ignorance: it does not speak the protocol, send a signal, or need a
second lifetime mechanism.

The same holds for `signal::adopt`: a window that takes an unowned server stops it
the same way, and it is also exit 0.

## The exit code says which ending it was

Every ending used to be anyhow's 0 or 1, which the supervisor cannot tell apart.

| code | meaning | supervisor |
| --- | --- | --- |
| 0 | asked to stop: `cancel`, a signal, a dead window, an adopted owner | leaves with 0 |
| 130 | a second, insistent ask (existing `asked()` path) | leaves with 130 |
| 2 | refused to run, and said why: another server holds the home, the config will not parse, the migration failed, `net::serve` returned an error | leaves with 2 |
| anything else | died without saying anything: a panic, a kill, `SIGKILL` | restarts it |

`2` is the one piece of vocabulary that has to be maintained forever, and it buys
the property that matters: a start that cannot succeed is not a crash, so a home
whose lock a live process holds is not retried every 200 ms into a log that grows
until the disk does. `gongga_server::main`'s error path is what exits 2 -- it was
the only place that had to change.

## Backoff, and no cap

200 ms, doubling, 30 s at most, and the delay resets whenever a child has lived
60 s. There is no attempt limit: a panic loop is worth retrying forever (it is
what the log is for), and `AGENTS.md` forbids turning a wait into a budget. A
server killed once an hour is therefore restarted at once every hour.

Each decision writes one line to the same log, prefixed `watchdog:`, so
`server.log` is still the only file a reader needs.

## What the supervisor does not do

- **No tokio runtime.** The fork happens before clap; `--help` and `--version` are
  answered by the child path, so a question does not build a process to print one
  line.
- **No `server.lock`.** The lock still belongs to the server, so `server.lock`
  still holds the pid of the process that serves. `evolve`'s `running_pid`,
  `status`, and the stale-lock takeover are unchanged.
- **No separate stdio.** The child inherits the console or `server.log` handle the
  supervisor was given, so the log's shape does not depend on which of the two
  wrote a line.
- **Ctrl-C is ignored on purpose** (`SetConsoleCtrlHandler` returning 1, `SIG_IGN`
  on unix). The child answers it; a supervisor that died first would leave a
  child nobody restarts.
- **The child is not detached from it.** Same console, same process group; only
  the supervisor is detached from whatever started it.

## The two pid semantics this changed

"Did this process start the server I am talking to" used to be pid equality, and
a second process breaks it in both directions:

1. **The greeting carries the supervisor.** `ServerInfo.supervisor` is
   `GONGGA_SUPERVISOR`. A window compares the pid it spawned with either pid, and
   `tui::run::whose_server` / `stops_a_server` accept both. Without it a window
   reads its own server as somebody else's and prints `adopted #…`.
2. **`cli::server::stop` kills the supervisor first.** `cancel` is asked for; if
   the server has not gone within five seconds it is killed -- and a server killed
   under a live supervisor is started again, which would make "stop what serves
   this home" false. Killing the supervisor first makes the restart impossible,
   and then the server is killed.

## What a supervisor cannot fix

`taskkill /F` on a supervisor takes its child with it (`/T` semantics on Windows),
which is the opposite of a soft stop. That is the same limit the signal path
already has: no process can prepare for its own murder, and the pending row is
written before a turn starts, not when the server leaves.

## Verified

`cargo test` is zero tests in this repository, so the evidence is a real run:
`scripts/watchdog_selftest.sh` (a scratch home under `.dev/selftest`, a port the
kernel picks, one verdict line).

| scenario | result |
| --- | --- |
| one server started by `gongga-server` | supervisor and child, the OS parent being the supervisor, and the pid in the log matching it |
| `taskkill /F` the child | a new pid within 200 ms, one `watchdog:` line |
| a second server on the same home | one refusal, no restart, same process count |
| `cancel` | both processes gone, no restart afterwards |

The three pid sources the script relies on are not incidental: a test server's pid
is read from its own home's `server.lock`, its supervisor from the OS parent
relation, and a python helper writes its own pid to a file. `$!` is an MSYS pid on
Git Bash and names a different process to `taskkill`, and the image name is shared
with every other server on the machine -- which is what `AGENTS.md` now forbids.
