//! Retries whose delay grows without a ceiling. See `docs/notes/protocol-retry.md`.

use std::time::Duration;

/// The delay before the `attempt`-th try, counting attempts from `1`.
///
/// # Invariant
///
/// No ceiling: how long a provider, a server or a link takes to come back is not
/// knowable in advance, so the ladder has no last rung. Saturation at
/// [`Duration::MAX`] is where the number type ends, not a bound anybody chose.
pub fn after(base: Duration, attempt: u32) -> Duration {
    let spent = attempt.saturating_sub(1);
    let factor = 1u32.checked_shl(spent).unwrap_or(u32::MAX);
    base.saturating_mul(factor)
}

/// One rung of that ladder: `delay` doubled, saturating rather than panicking.
///
/// # Invariant
///
/// `Duration * 2` panics on overflow, so every ladder step is taken here.
pub fn doubled(delay: Duration) -> Duration {
    delay.saturating_mul(2)
}

/// Wait out `delay`, however long it is.
///
/// # Invariant
///
/// The deadline is asked for, not assumed: `Instant + Duration` panics once the wait
/// leaves the clock's range, and a wait this clock cannot name is one no process
/// outlives, so it is spent as a future that never becomes ready. Dropping this
/// future -- a `select!` arm that lost -- cancels the wait where it stands.
pub async fn sleep(delay: Duration) {
    match tokio::time::Instant::now().checked_add(delay) {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}
