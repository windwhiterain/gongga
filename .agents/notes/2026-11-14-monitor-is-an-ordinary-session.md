# A monitor is an ordinary session

**The verdict (user, 2026-11-14):** delete the `adopt` mechanism and the name
restriction; a monitor has no special mechanism. It came from a report that
`set_session` refused `@monitor/disk`, so a monitor's configuration could not be
changed at all.

## What went

- `monitor::reserved` and its call in `set_session`: no name is reserved. The
  harness knows its sessions by the constant it created they under.
- `monitor::adopt`, `candidates`, `allowance`, `Catalog::speaks_responses`,
  `quota::of`: nothing picks a model for a monitor or switches it on failure.
  `sessions.monitor_attempts`, `disk.attempts`, `disk.wake`, `quota.attempts`
  and `quota.wake` went with them; the column is dropped where the schema heals
  (`store::migrate`), not by a version bump -- see `docs/notes/server-migrate.md`.
- `prompts::MONITOR_PROVIDERS_AGAIN` (the retry's template) is no longer read.
  The template itself stays in the content: a prompt *name* is content shape.

## What replaced it

- `commands::create_session` = `set_session` with a name, so a monitor is born
  with `DEFAULT_SESSION_MODEL`, the server's cwd and the default tuning.
- A monitor's own failure is skipped by identity (`failure.name ==
  PROVIDERS.session`), so it is never handed a report about itself.
- One cursor per event kind (`disk.event`, `quota.event`); a handover that cannot
  happen advances it with one `Event::Log`, one the session refuses leaves it.

## What this costs

A monitor whose model breaks no longer switches by itself: it stays in `error`
for a person. With no reserved namespace, a person who names a session
`@monitor/disk` gets the disk reports. Both are in `docs/notes/server-monitor.md`.

## Verified

`scripts/monitor_selftest.sh` and `scripts/disk_selftest.sh` (both rewritten for
this shape) PASS on `07426b9c`, and a copied version-5 home opens with
`sessions.monitor_attempts` dropped, `monitor_seq` kept and its 32 sessions intact.
