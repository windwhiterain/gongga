//! The pure halves of the adaptive visible limit: what counts as a capacity
//! refusal, and how the limit moves.

use crate::llm::capacity_refusal;
use crate::memory::{grown_limit, halved_limit};

#[test]
fn a_stated_capacity_refusal_counts() {
    assert!(capacity_refusal(
        "HTTP 503 from http://127.0.0.1:8312/v1/responses: too many queued requests"
    ));
    assert!(capacity_refusal(
        "HTTP 507 from http://127.0.0.1:8312/v1/responses"
    ));
    assert!(capacity_refusal(
        "model request failed: CUDA out of memory. Tried to allocate 20.00 MiB"
    ));
    assert!(capacity_refusal(
        "the engine reported: not enough device memory for the kv cache"
    ));
}

#[test]
fn other_minutes_do_not_count() {
    assert!(!capacity_refusal("HTTP 500 from http://x: boom"));
    assert!(!capacity_refusal("HTTP 429 from http://x: slow down"));
    assert!(!capacity_refusal(
        "error sending request for url: timed out"
    ));
    assert!(!capacity_refusal("the stream ended without a completion"));
    assert!(!capacity_refusal("HTTP 400: invalid request"));
}

#[test]
fn a_refusal_halves_but_never_reaches_the_tail() {
    assert_eq!(halved_limit(115_776, 1), 57_888);
    assert_eq!(halved_limit(57_888, 1), 28_944);
    // The ordinary tail is the floor: masking cannot remove it.
    assert_eq!(halved_limit(8_000, 6_000), 6_000);
    // A one-token limit cannot become zero.
    assert_eq!(halved_limit(1, 0), 1);
}

#[test]
fn success_grows_by_half_and_stops_at_the_ceiling() {
    assert_eq!(grown_limit(28_944, 115_776), 43_416);
    assert_eq!(grown_limit(100_000, 115_776), 115_776);
    // Never past the ceiling, and never a shrink when it is already there.
    assert_eq!(grown_limit(115_776, 115_776), 115_776);
    assert_eq!(grown_limit(1, 1), 1);
}
