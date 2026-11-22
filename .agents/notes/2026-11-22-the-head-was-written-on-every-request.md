# The head was written on every request

## The fault

`Store::prompt` answered `None` whenever the five head rows did not deserialize,
and `prompt_in_force` answers `None` by writing a head. A row this build could not
read was therefore the same signal as a row that was never written, and the
answer to it was another copy -- on every request, for as long as the session
stayed in use.

Measured on the home this was found in:

| session | requests since 11-22 03:25 | head copies |
| --- | --- | --- |
| `keep_alive` (225) | 134 | 136 |
| `linux` (396) | 61 | 62 |

Every pair lined up within 15-33 ms: a head write immediately before each
`llm_attempt`. 1124 head rows across the home, 792 of them redundant copies.

## What made the row unreadable

`ship/migrate/6-to-7.py` split a version-6 prompt into four rows with
`prompt.get(key)`, so a key the version-6 row never had came out as an explicit
`null`. `Prompt` declares those list fields with `#[serde(default)]`, and
`default` covers a key that is absent, not one that is null. Every session whose
head predates 11-21 03:25 carries it.

## The fix

- `prompt` reports an unreadable head, naming the fields, instead of answering
  `None`.
- `Prompt`'s list fields read `null` as empty.
- `append_prompt` refuses a second row in a slot it already holds one in, as
  `write_head_once` already refused a second tool set.
- `prompt_in_force` reads the head back when its own write is refused.
- A slot holding several rows is read as its newest.
- `7-to-8` drops the older copies and keeps the newest of each slot.

## What it cost besides the bytes

- seq stopped identifying a row: 18 `(session, seq)` groups held more than one
  row, and `get_messages --range 1:1` returned a hundred of them.
- The duplicated rows cannot be deleted through any ordinary API: the
  `context_cut_tail_only` trigger only allows a delete at the tail. Only a
  migrator that drops the trigger can reach them.

## Measured

`scripts/head_once_selftest.sh` (five turns over a live server, then a head
planted with a null list):

| build | null head, turns over it |
| --- | --- |
| before (`4ba141cb`) | 1 -> 2 |
| after | 1 -> 1 |

Migration against a copy of the live database: 1124 head rows -> 332, and
session 225 kept row 388591, the newest one.

## Left alone

The transcript copies `handoff` makes into a mentor put ordinary rows in slots
1-5, so a mentor's own head later lands past them (`kv-mask/mentor` keeps its
instructions at 2-7). Those sessions are readable now that `null` reads as
empty; repairing their layout means renumbering transcript, which no step here
does.
