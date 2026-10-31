# Heir training and schema gate

## User decisions

- Fix the heir script and add a gate against the schema drift found in quick1.
- Encourage batching independent tool calls and cascading dependent commands.
- Replace completion-oriented handover prose with immediate heir training. The heir continues the current work; a commit, clean tree, or green suite is not a prerequisite.

## Implementation

- Run the existing heir script checks against the schema extracted from the server's `store.rs`.
- Run that gate from the existing ship publication script.
- Preserve both wire and display payloads, plus `call_id`, when copying transcript rows.
- Keep internal database and event names unchanged; change only model-facing terminology.

## Evidence

The quick1 gongga1 run hit `NOT NULL constraint failed: messages.display` at 2994.7 seconds and completed its handover at 3403.5 seconds only after repairing the installed script.

The new gate reproduced that exact error before the fix: four checks failed, including blank reader text and the destructive copy path. Windows also required falling back from the Store's inactive `python3` alias to the installed `python` interpreter.

After preserving `content`, `display`, and `call_id` on every copied row and writing both payload shapes for new mentor rows, all five checks passed against the server's extracted schema.

The shipped instructions now batch independent read-only calls and cascade dependent commands with `&&`. The context notice, size hint, heir guide, and script responses tell the agent to start training an heir immediately and continue the current work without requiring a commit, clean tree, or green suite. A sixth gate check protects these prompt requirements; both standalone and real-schema runs pass all six checks.

The ship release is published as `4e6befbc08595e4e34e9f495c6b16ae8bcc4d304`; the manifest, submodule, and remote main agree. The new pin builds successfully, and 12 prompt tests, 12 ship tests, and 6 agent tests pass. The release self-test initially exposed Git's blocked local submodule transport; allowing `file` only for that scratch clone made all 12 publication checks pass.
