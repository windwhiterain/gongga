# server/quota.rs

## quota.rs

SPEC's `active_quota` block says what the few most recently asked providers still
allow, and says it gets updated at every step's end. The numbers are not this
server's to compute: a provider knows its own balance, and the roster already
knows how to ask it -- `[[provider.quota_query]]` in `providers.toml`, with a
`script` that prints one JSON reading. This module is the part that runs that
script and files what it said under SPEC's own labels.

Nothing here speaks HTTP. The roster's `url` and `jsonpath` fields describe an
endpoint for a human reading the file; the script is the machine-readable half,
and running a script is the wheel this repository already has (`script.rs` spawns
one the same way). A query with no `script` is skipped rather than half-read, and a
provider all of whose queries are like that has no row at all -- see `run`.

## READER_TIMEOUT

A hang guard, not a budget. One provider's endpoint that never answers must not
keep a child process alive for the life of the server, and the row is worth less
than the turn it hangs behind.

## Last

A reading is filed by provider, because SPEC's block is several providers at once,
and because a choice between providers is a comparison. `requested` is the other
half of the block: the ordinal of the last request sent to each provider, with
`next_request` handing the ordinals out. One counter for every provider on purpose
-- "asked most recently" is a comparison *between* providers, so two providers'
numbers have to come from the same sequence.

`in_flight` is what keeps a fast turn from starting one child per step: two steps'
ends inside one slow read are one question about the same provider, and the second
one answers with what the first is about to publish. It is keyed by provider
because one read covers every query that provider lists.

`answered` is the other half, and the reason it is keyed by **endpoint**
(`<provider>/<query id>`) rather than by provider: the interval is a statement
about one URL. Minimax lists three readers for one provider, and a provider-level
clock would let the M Plan reader -- the one that answers -- decide how often the
pay-as-you-go wallet may be asked.

## readings

A window that attaches between two steps must be told the rows that are already
known, or the block is blank until the session happens to spend something. The
greeting carries the sessions and the catalog; this is the same idea for the
measurements that are not part of either: `readings` hands back every row this run
has, ordered by `request`, and the greeting sends them one event at a time.

The order is computed, never inherited from the arrival: an endpoint that took ten
seconds to answer belongs where its request put it, which is why `readings` sorts
instead of reusing the order the answers landed in.

## note_request, requested

`note_request` is called where the request is made -- `llm::stream` -- and never
where the answer is filed. SPEC orders the block by the last request, and a step
that has been waiting a minute has still been waiting since it started: stamping at
its end would put a slow provider behind one that was asked later and answered
sooner.

`requested` is the lookup `publish` uses to stamp the row it is about to publish;
0 means no request has gone to that provider in this run.

## publish

Called at a step's end, from inside the turn's stream loop, and the reason it
spawns instead of awaiting: reading an allowance is a network call through a child
process, and a turn that waited on a provider's billing endpoint before its last
token could be reported would be a turn that stalls for something nobody asked
for. The step goes on; the row arrives when the answer does.

`interval` is the cadence in `[server] quota_interval_secs` (default 60s, `0` asks
every step end). A step end is seconds apart from the one before it, and a balance
is not: asking a billing endpoint once per step
is a request per step per provider, which is what a provider rate-limits, and the
rows SPEC asks for do not change in between.

## read, read_in

One is the live roster, the other is a roster a test hands in. The split is what
makes the whole path testable without a network: a temp `providers.toml` naming a
temp script is read exactly as a real provider's is.

## interpreter

`config::detect_python` runs child processes to find Python, so it is asked once
and its answer kept. A reader that has no interpreter to run is a note in the
row, never a panic.

## run

A provider whose queries name no script is `None`, not an empty reading: most of
the roster's entries publish an allowance this server cannot read (Qwen's says
`available = false`, several name only a console URL), and a row saying "no reader"
at every step of every turn is noise about a fact nobody can act on. A provider
that *does* publish a reader, and whose reader fails, is a row with a note -- that
one is worth saying, because the allowance exists and this machine could not read
it.

The roster's queries are tried in file order and the first answer is the one
shown. That is the same rule the roster uses for everything else -- file order is
preference order -- and it is what lets one provider carry two products: the
Minimax entry lists the M Plan reader first and the pay-as-you-go wallet second,
so a subscription key reads the plan and a pay-as-you-go key falls through to the
wallet without either naming the other.

A query that fails is remembered rather than reported immediately: a provider whose
first reader needs a credential this machine does not have should still get the
second reader read.

## kept, remember

What one endpoint said, stamped with when it said it, so a step end inside the
interval reuses it instead of asking again. Two facts follow from that, and both
are deliberate:

- **A failure is kept like a reading.** An endpoint that just answered 429, or did
  not answer at all, is exactly the one a retry per step must not hit; the note it
  produced stays the row's note until the interval is up.
- **The answer is indexed by endpoint, so a kept answer cannot be mistaken for a
  fresh one elsewhere.** The roster's preference order still decides which answer
  is shown: a kept reading of the first query returns before the second is looked
  at, and a kept failure falls through to the next query the way a fresh one does.

The clock is `Instant` and the comparison saturates: a monotonic clock that stepped
back is not a reason to panic on a turn's path. `kept` with `interval` of zero is
never fresh, which is how `0` keeps meaning "ask every step end" without a branch
of its own.

## ask

The key goes in the environment, which is where the roster's own scripts look for
it (`script_env`). Argv is the one place it must not go: on this machine every
process can read another's command line, and a balance is not worth a leaked key.

`kill_on_drop` and the timeout are the same promise as the rest of the harness: a
child process does not outlive the question that started it.

## Reading, Window, parse

The shape a reading arrives in, kept as the script's own numbers and names. Which
window is "month" is the mapping's business, never the reading's -- the same script
serves a console report that labels windows its own way.

`parse` reads the JSON the reader printed. The whole text is tried first and the
last line second, because a reader may say something before its answer, and a
reading pretty-printed over several lines is still one reading.

## window_of

Providers spell the same three periods differently: `monthly`, `weekly`,
`five-hour`, `5h`, `current` (Minimax's current interval *is* the five-hour one),
`rolling`. The mapping is the only place that knowledge lives.

An unknown name is not guessed at: a provider inventing a fourth period leaves that
number out of the row rather than filing it under a window it is not.

## Reading::of

SPEC's four figures, from what the reading says:

- a window with a stated capacity is a share of it, which is SPEC's `n%`;
- a window without one is a count of what is left, which is SPEC's `credit n` --
  the GOAT plan's monthly credits are money-shaped but unscored, and calling them
  a percentage of nothing would be a lie;
- a reading with **no** windows is a wallet, and its one number is `money`;
- `error` from the reading is the row's `note`, because the provider's own words
  about a spent window are the most useful thing in the reading.

A plan that reports windows is never also a wallet: its `remaining` is the
tightest of its own windows, which the shares already say.

## currency_of

The roster's `unit` is what says whether a number is a share or a currency, so it
is also what says which symbol the money is in. `cny-left` is `¥`, `usd-left` is
`$`; anything else is not money at all.

## Every provider, not only the last one

`readings()` is every provider read in this run, ordered by its last request, which
is what SPEC's `active_quota` block draws from. `of(provider)` is one of those rows
looked up by roster id, and it exists because choosing between providers is a
comparison: SPEC's monitor picks a flash model by what each provider still allows
(`server-monitor.md`), and one reading says nothing about the others.

The table is in memory only. A reading is a measurement of a moment, not a fact
about a home, so it is not written anywhere and a restarted server learns them
again at the next step end.
