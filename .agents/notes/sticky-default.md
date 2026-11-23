# Sticky must not fill a defaulted struct field

User report (2026-11-23): `new_session`'s `live_compact` was filled from sticky even
though SPEC gives it `= default()`; sticky fills only arguments **without** a default.

## Cause
The client decides "has a default" from the published schema's `default` marker
(`argschema::default`). The schema dropped SPEC's defaults:
- `session_config.rs::object()` hardcoded `default: None`, so `live_compact`
  (SPEC `live_compact: LiveCompact = default()`) carried no default.
- the `LiveCompact` sub-fields `enabled`/`interval_tokens`/`mode` were built with no
  default though SPEC gives each `= default()`.
With no default seen, a `LiveCompact`/`model_config` entry stored from an earlier
`set_session` filled `new_session`'s omitted `live_compact`.
Also `window_state.rs::value_of`'s struct branch recursed **before** the default
check (which only guarded leaves), contradicting `fill_struct`'s invariant "a field
with a default is never filled at any depth".

## Fix
- `object()` takes a `default`; `live_compact` is marked `default()`; the three
  LiveCompact sub-fields are marked `default()` per SPEC.
- `value_of` skips a struct field that is omitted and defaulted (mirrors the leaf).

## Verification
- `mbx build` 0, `mbx fmt --check` 0, `test-layout-check` 0, client lib 7/7,
  server lib 40/40. comments-check: changed files clean; the only violations
  (`net.rs`, `remote/runner.rs`, `tools/detached.rs`) are byte-identical to main.
- L3 `scripts/sticky_default_selftest.sh` (real server + real client submit pipe):
  a window states `live_compact.interval_tokens=1234` on `s1` (kept), then
  `new_session{"id":"s2"}` omits config — the filled config carries `model` but NOT
  `live_compact`; `s2` interval is null. Negative control (fix stashed): `s2`
  interval=1234, test FAILS. Data: `.dev/verify/<sha>-sticky-default.json`.