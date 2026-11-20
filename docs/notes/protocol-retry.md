# protocol/retry.rs

## One ladder, and it has no last rung

`after(base, attempt)` is the delay before the `attempt`-th try: `base` doubled once
per attempt already spent. `doubled(delay)` is the same step taken from a value a loop
already carries, which is how the bridge's redial ladder and the watchdog's restart
ladder grow theirs. The client polls at a fixed interval instead
(`docs/notes/client-runtime.md`).

Every one of them used to stop at a ceiling -- `[model] retry_ceiling_ms`,
`RECONNECT_MAX`, `REDIAL_MAX`, the watchdog's `MAX_BACKOFF` -- and the user's rule is
that the harness sets no such budget: how long a provider, a server or a link takes to
come back is not knowable in advance, so the ladder's only end is the caller's cancel.
The caps are gone; the doubling stayed. `max_retries` is a different subject and is
unchanged: it counts attempts, and `0` already means no ceiling there.

Saturation at `Duration::MAX` is not a cap somebody chose. It is where the number type
ends, about sixty doublings from a second, and a wait of that size is one no process
outlives either way -- which is why saturating is the honest arithmetic and a panic on
overflow is not.

## sleep

`Instant + delay` panics once the deadline leaves the clock's range, so the deadline is
asked for with `checked_add`. A wait this clock cannot name is spent as a future that
never becomes ready: a delay past the clock's horizon is one the process will not see
end, so "never" is the honest answer rather than a panic.

That is not a substitute for a cancel. Every caller puts this future beside the thing
that ends a wait for real -- the request's `CancellationToken`, the window's keyboard, a
closing console -- and dropping the future cancels the wait where it stands. It is what
keeps `interrupt_session` responsive while a very long delay is pending.

The watchdog's `std::thread::sleep` is deliberately not routed through here: that
process has no runtime, and std saturates such a wait against the platform timer instead
of panicking.
