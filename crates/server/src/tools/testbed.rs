//! Waits for a condition, not a clock: a safe sleep is most of the suite's time.

use std::time::Duration;

/// Polls `cond` every 10ms until it holds or `within` passes, then asserts it.
pub(crate) async fn wait_until(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if cond() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("still waiting for {what} after {within:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
