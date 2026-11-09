# scripts/multi_client_selftest.sh

Step 8 of shape 2. One server, two `gongga-client` children at once: the
client-to-server leg is many-to-one and remote-capable, while each UI owns
exactly one child. Nothing here is a unit test: every assertion is about real
processes, a real UDP link and files on disk.

## What it proves

- Both children dial the same address and key and report the same server pid in
  their `Server` frames, with `ours=false`. The server is started by the script
  without `--parent`, so it is `Server::Unowned`: no window may cancel it.
- A session created through A (`from-a`) and one created through B (`from-b`)
  are both readable by a third, one-shot dial. They live in one server's state,
  not in two servers that happen to share a port.
- Window state stays with its own child: A's sticky session appears in A's
  `client.toml`, B's in B's, and neither leaks across.
- A sends `quit` and leaves. `server.lock` still names the same pid, B is still
  alive and can still write to that server, and a one-shot dial still answers.
  This is the `--parent` ownership rule at the end of a real lifetime.
- A third child points at a non-loopback address without `--client_only`. It
  neither spawns nor stops a server: `stop()` and `spawn()` both return at
  their first `starts_here(&addr)` check.
- Every line on both children's stdout parses as JSON.

## Shape of the run

The shell owns the server's lifetime and the verdict. A Windows-native Python
driver owns the three child processes, speaks `UiIntent` frames, reads
`RenderMessage` frames with a queue, and prints one `ok` / `FAIL` line per check.
It holds no state the client could not hold itself.

The third observer is the `gongga-cli` shim the server writes into
`<home>/bin/gongga-cli`. That is a POSIX `#!/bin/sh` file, not an `.exe`: on
Windows the driver starts it through `bash`, and must never probe it with `-x`.
Paths handed to the driver go through `cygpath -m`, because `/c/...` is an MSYS
path that Windows-native Python cannot open.

Both `--home` and `--client-home` are given to every child. `client_home_of()`
reads `--client-home` / `GONGGA_CLIENT_HOME`, not `--home` / `GONGGA_HOME`;
leaving it out reads the operator's real `~/.gongga-client` roster. The script
also pins `GONGGA_PROVIDERS_CATALOG` to its own fake roster.

## Cleanup, and why it is not the usual one

`AGENTS.md` says a test server's pid comes from the test home's `server.lock`.
That is necessary but not sufficient here: the server supervises itself, and a
watchdog restarts a killed child after 200 ms. Killing only the lock pid leaves a
fresh server holding the home, so the next run cannot remove its directory.

The server's own log prints `supervisor=<pid>` on its "gongga server starting"
line. That pid is the watchdog, and `taskkill /T /F` on its tree takes the child
with it. `stop_servers()` kills that tree and the lock pid, and is called both
at the start of a run (to heal a previous run that died before its trap) and at
the end. Two consecutive runs pass without any manual cleanup.

Everything else follows the L3 rules: a temporary home, a port from the kernel,
an explicit key, model variables scrubbed from the server's environment,
readiness read from the log's listening line, and one
`PASS · <seconds>s · multi-client · <sha>` at the end. `-v` prints all checks.
