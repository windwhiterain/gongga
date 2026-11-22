# Dreamer observations: cold first request and a self-delete deadlock

Findings from the live `kv-mask` dreamers on 2026-11-22. Code and content fixes
are in `334da9bf` plus content commit `a823a54`.

## First request hit only 22%

`kv-mask/dream/1795314481937` (created 10:28:02) had `input=161058, cached=36096`
on its first request; its second request was 99%. The clone's first 95 message
rows are byte-identical to the source, but rows 96+ differ: the source rewrote
its tail after the clone was taken (live compact / continued work), so the
provider only had a cache for the shared prefix. This is a snapshot race, not a
cache-tier change: the source and clone have the same `head_bytes=103703`, and
neither session has a `cache_tier` set.

Fix idea not yet chosen: keep the dreamer on the source's prefix by cloning at a
quiet boundary and not letting the source compact during the dreamer's first
request, or accept the one cold request as the price of a parallel copy.

## Self-delete deadlock (fixed)

`kv-mask/dream/1795277629441` spent 120s in `gongga_cli del_session` on itself,
then was cancelled. `del_session` runs `finish_delete`, which waits on the
session gate; the dreamer's own turn held that gate. This was already documented
in the old `server-dream.md`: a dream deleting itself mid-turn deadlocks.

Fix: `clone_session` now takes `dream_of=true`, and `@skill(dreamer)`'s `dream()`
passes it. The server discards the copy at its true turn end. The dream notice
no longer asks the clone to call `del_session`.
