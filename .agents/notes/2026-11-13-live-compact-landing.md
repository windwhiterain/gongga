# Landing live_compact settled/next (protocol 9 -> 10) + channel notice_end

Branch feature/live-compact-two-part was behind main; main already has relay_for_channel
(the installed-copy fix). User: finish the unfinished feature, then merge.

Plan (content-first, one protocol notch):
1. ship/: checkout main (carries the settled/next config edit), bump ship/ship.toml 9->10,
   scripts/ship.sh -m ... (commit, rebase onto origin/main, push). Content shape change:
   live_compact summary -> settled(required)+next(optional).
2. outer: discard dirty Cargo.lock (main already has relay libc+which); rebase onto main
   (channel.rs: feature notice_end region vs main relay_program region -- disjoint).
3. outer: crates/server/ship.toml 9->10.
4. gates: build, fmt, comments(baseline), test-layout, skill_contract, ship --check, channel selftest.
5. squash to one commit (store.rs carries both features, cannot split by file), merge main, delete branch.

Effect already bench-verified (lc2 resolved 2/2, origin fix). This landing is the protocol
coordination the 0eeea18 record deferred. Do NOT touch SPEC.md (user WIP).
