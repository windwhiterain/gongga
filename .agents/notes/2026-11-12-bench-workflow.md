# SWE-bench workflow guards (built 2026-11-12)

Installed in the bench repo `lichen-vm WSL /root/claw-swe-bench` (NOT the harness):

- `workflow.sh` (0755): the guarded entry. Prints bench location + "read AGENTS.md",
  the provenance rule, and the case-by-question list; runs the free gate
  (`compact-bench.sh --case write_a_file --fake`); refuses paid arms without `--paid`.
  Delegates to compact-bench.sh (egress gate, arms, verdict already live there) — no duplication.
- `AGENTS.md`: new top section "起点与证据" — bench is separate from the harness;
  conclusions only from raw artifacts here (predictions.jsonl, gongga.db), never from
  the harness `.dev/verify` summaries; use designated cases; free gate before paid.
  Backup at `AGENTS.md.bak`.

Validated: `./workflow.sh` free gate PASS (~54s): egress 7 probes pass, write_a_file
resolved 1/1, verdict `PASS · build 21ce4d4`, network/proxy cleaned up.

Guards map to the mistakes made this session:
- didn't find/read the bench -> entry prints location + forces AGENTS.md
- trusted harness .dev/verify summaries -> provenance rule (raw artifacts only)
- picked django-11880 (not a case) -> case-by-question list
- skipped free gates / jumped to paid -> free gate first, --paid + ask user

Still open: the harness AGENTS.md carries bench lore and .dev/verify holds bench
summaries — the discovery/provenance misdirection. Proposed boundary fix pending user OK.
Peer bench source /root/gongga-local has the settled/next Rust edits applied but NOT
rebuilt (binary still 21ce4d4); that is deferred-experiment WIP.
