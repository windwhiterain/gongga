# server/session_config.rs

SPEC's `SessionConfig`, the one object `new_session` and `set_session` take, flattened
into the flat settings the handler reads.

## One object in, flat names out

`flatten` is the only reader of `--config`. Each level keeps what it omits, because an
absent key never reaches the handler: `model_config` and `workspace` are objects of
optional fields, and `SessionConfig` is an object of optional objects. A delta that
names `workspace.cwd` and nothing else changes the directory and nothing else.

A stated `null` is a value, not an absence, for the two fields that take it: `remote`
and `cache_tier` are cleared by saying `none` (the one string spelling) or `null`.

## A model is spelled two ways

SPEC spells `Model := {provider, model}`. The session's row, and the sticky entry a
window keeps, spell the same value `provider/model` -- one word, no object. Both are
read here (`ModelValue`), so a client can hand over whichever it holds; a bare name
is the provider's own, and a provider without a model is refused by name rather than
guessed at.

## The schema

`schema` publishes the shape a client completes inside `--config`: the field names,
which completion source each leaf offers, and -- through `accepts` -- the closed sets a
model refuses (`thinking` levels and `cache_tier` names come per model). The leaf names
are also the sticky entry names the window fills, so a leaf renamed here renames the
entry that fills it.

Each leaf also states its **default**, in SPEC's own spelling (`provider_default()`,
`70%`, `none`, `.`, `false`), and a field without one states nothing. That is what the
client reads to decide whether an entry may fill it: SPEC's `# re-default if model
change` on `thinking`, `ctx_limit` and `cache_tier` is a promise that the type decides
them, and a value a reader typed days ago may not pre-empt it. Only `model_config.model`
and `keep_alive` have no default here, which is exactly the set a sticky entry may
fill. `defaulted` is the one place a field is marked, so a default SPEC states and a
default this schema forgets cannot drift apart unnoticed.
