# Live compact: the stored tool set, the no-op, and the ratio it bought

Follows `.agents/notes/2026-11-07-live-compact-effect.md`, which measured the
problem: compaction controlled the context but cost 13.8% more and ran 9.9%
slower. This round fixes the head, decides what an unasked-for call does, and
rewrites the guidance the summary is written against.

## The head was assembled per request

The `live_compact` schema was added to the tools array only while a durable
request was outstanding, and withdrawn after its commit. `tools` is serialized
before `input`, so that entry and exit sat inside the cached prefix and
invalidated the whole conversation at every checkpoint: 12 breaks per 6 cycles,
182,917 uncached tokens (the six pre-call ones, 155,298, are what a stable head
could have saved).

The tool set is now a stored row (`docs/notes/server-tools.md`), stamped only
when one of four stored facts differs: the model, the peer, whether a compaction
origin has opened, and the build id. The build is in the comparison because a
build may rename or reshape a tool, and a session that outlived it must not keep
advertising a schema this server no longer has.

## A call nobody asked for is a no-op

It used to be an `ERROR` result, which is what invites a retry loop. It now
returns a successful result saying no checkpoint was outstanding, appends no
checkpoint row, bumps no generation, resets no baseline, and writes no message
beyond that result (`docs/notes/server-live-compact.md`).

## The guidance: a citation embeds, so do not cite what you just read

The first wording told the model to cite a file as `@file(<path>)` instead of
pasting it. In the A/B the model cited `@file(evidence.txt)` -- the file it had
just read -- and clause 4 embedded that file's whole content into the summary:
97,404 characters against an 878-character summary, re-sent with every later
request. The arm measured 424,275 uncached tokens against the disabled arm's
180,798, and 1.33 CNY against 0.91: the opposite of the intent.

The wording now says what the embedding does. Summaries fell to a mean of 154
tokens per checkpoint, against the 722 the earlier run recorded.

## Only explore-born sessions compact

The user's decision closes the clause-2 question the earlier note left open: live
compaction belongs to a lineage that planned. A session created directly in work
never opens an origin, so it has no baseline, is never asked for a summary and
never carries the schema; explore -> work at plan finish is the only door. The
work-born branch this note's measurements were taken with is removed.

## The A/B and the sweep

`deepseek-official/deepseek-flash`, interval 20,000, ten phases of sequential
`read_file` calls over a 200-line `evidence.txt` whose first line is
`IMPORTANT-MARKER-7719`, then one recall turn without tools. Five arms, one home
and one port each, run concurrently on lichen-vm.

| arm | requests | uncached | cost CNY | peak input | latency | checkpoints | summary tokens | compaction latency |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fixed | 118 | 201,746 | 0.6057 | 37,815 | 147.0s | 7 | 154 | 1.29s |
| main before this round | 115 | 373,872 | 0.9678 | 42,062 | 157.0s | 7 | 405 | 1.63s |
| live compact disabled | 111 | 180,380 | 0.8869 | 191,816 | 154.5s | 0 | -- | -- |
| sweep 10k | 117 | 207,952 | 0.6313 | 31,281 | 171.3s | 12 | 172 | 1.49s |
| sweep 40k | 119 | 203,182 | 0.6898 | 58,304 | 166.1s | 4 | 53 | 1.40s |

Against the disabled arm: uncached 1.118x (was 2.00x), cost 0.683x (was 1.138x),
peak 0.197x, latency 0.952x. Against the pre-change build on the same workload,
the stable head removed 172,126 uncached tokens, 46% of that arm's uncached input
-- the money the recorded 182,917-token decomposition predicted.

The sweep says the default is right where it is: 20k costs least and its peak is
within 21% of 10k's; 40k buys a lower peak only by compacting rarely and paying
more. Nothing here changes the default.

Every arm recalled `IMPORTANT-MARKER-7719`. The line counts they state vary
between 198 and 200, and 200 is right: `read_file` truncates each block inline and
spills the rest to a file, so the model infers the total rather than reading it.
The earlier note recorded the same variance; it is a property of the workload, not
of compaction.

Artifacts per checkpoint are 2.14x the summary's own tokens on average (a summary
travels twice: the call's arguments and the result), and the retained round is 86%
of the post-compaction context. Both are bounded; neither needs a code change.

## A cancelled turn does not end

`turn_end` carries the answer, so a cut turn publishing it is a lie every
subscriber reads as a completion. `cancel` interrupts a turn; it does not end it.
A cancelled turn now publishes `turn_interrupted` with the same payload minus
`answer`, and `turn_end` is left to Done and Failed. A script that waits on
`events --kind turn_end` alone waits forever on a cut, which is how this was
found: a bench driver and the polity watch both read a cut as a finished turn.
