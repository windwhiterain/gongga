- when designing new feature, search any existing wheels. 
- setting any limit/budget is forbidden.
  - No step count, no turn wall-clock, no retry ceiling, no output-length cap, no round
    limit, no concurrency cap. Not "a generous one" either — any number is wrong in advance.
  - A turn may end for exactly three reasons: the work is done, the user cancelled, or a
    request genuinely failed. All three must be reported (status, log, transcript). Silent
    truncation and pretending-to-be-done are bugs.
  - Context compression (`compress_above_chars`, `keep_recent`) and stall watchdogs
    (`request_timeout`, `idle_timeout`) are not limits: they keep a long run alive. The test
    is "does this make the agent do less work".
  - If a threshold is ever truly needed, ask the user first, justify it in `DESIGN.md`, make
    the default "unlimited" (0), and keep it overridable.
