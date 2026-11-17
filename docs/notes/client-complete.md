# cli/complete.rs

The completion: the vocabulary a front-end is answered in, and the one place the
line's JSON is read.

## complete.rs

`Item` is one option: `label`, `detail`, `new_text`, `snippet`, and `range` when
its token is not the answer's own. `Offered` is one answer: `subject` (what the
popup says it is asking for), the range every item replaces, and the items.
`Completion` is the open popup: the line and caret it answers, the same range,
the items, and the selection.

**A JSON candidate rewrites the token the caret is in**, and reads nothing after it. The
span is `json_place::token_at`'s: the quoted string the caret stands in, or the bare run
around it, in the line's own offsets. What the operator wrote behind the caret is theirs —
the closing braces an unfinished object owes are never written by a candidate.

**A name writes the whole member from the caret, its separator at the tail**:
`"name":<value>,$0`, the value being `{}` for an object field, `""` with the caret inside
for a string leaf, and nothing for the rest. A name is offered in exactly two places — where
a member begins (`{` or `,` behind the caret, `after_opener`) and inside a name the line is
typing — because anywhere else the line has already ended its member, and a name written
there would stand beside it with no separator in front of it.

**A value takes the comma that ends its member with it.** Its range grows over that comma
and the candidate writes it back at its own tail, so the caret lands after the separator
rather than in front of it. It is the one byte a candidate reads off the far side of the
caret, and it is the separator the member being filled already owes; the range is widened in
the line's own coordinates, which is where `token_at`'s tail-relative offsets stop being
usable.

**A word candidate still replaces the word it completes.** `Completion::apply` takes the
item's own range or the answer's, never part of either, and a word never doubles the space
it ends with. That is the flag form and the `@` references.

**A snippet stop is `$0`..`$9` or `${n}`/`${n:word}`** (`expand`). The lowest stop
decides, at its first appearance; a snippet stating none leaves the caret at the
end. A `$` naming none is text.

**Accepting is `Completion::apply`**: the item's own range or the answer's, never
part of either; a word never doubles the space it ends with.

## Where the cursor stands: json_place.rs

`close` writes what a half-typed line still owes — the closing quote, `:null`
after a name, `null` where a value belongs, and the closers — so that
`jsonc-parser` reads it as one document. Strings, escapes, nesting and lists are
the crate's; this layer only says where the cursor stands (`Place`), which token
it is on, and what the objects around it already state.

**A place, not a parse.** A cursor past the end is clamped, never refused, and
text that does not close into a document is `None`, which the answer reads as
"offer nothing".

**Only the end is written.** Every byte of the line keeps its offset, and a range
read off the closed document is a range in the line. `Place::end` stops at the
line's own end and never runs past it.

**A bare word being typed is stood in for.** `tru`, `1`, `t` in value position are
not JSON, so the document is read again with an equally long stand-in (`"x"`,
`"xx"`, `0`) at every bare token. Offsets do not move, and the item then replaces
what the operator typed while the fragment stays the operator's own text.

**The span is the token, never the punctuation behind it.** A colon or a comma the line
already holds stands where the operator put it, so re-accepting a name over one written
name leaves that name's own colon in place; a value is the one place a candidate takes its
separator back (`comma`).

**The options**: loose only where a half-typed line is ordinary (a trailing or a
missing comma). Nothing looser, so what completion reads is what the server reads.

## The command is one JSON object

A command line spells its arguments as one JSON object right after the name:
`/set_session{"id":"abc","config":{...}}`. Completion offers this form alone — the
flag/positional spelling still parses (a front-end's own click lines use it), but
the popup never offers it.

**The root shape.** The command's published arguments become one object schema
(`root_schema`): each argument a field, a structured argument (`ArgSpec::structure`)
a nested object. Completing inside the object is then the same field-name /
leaf-value completion at every depth.

**What it inserts.** A name is `"name":` with the member's value — `{}` when the field
holds an object, `""` with the caret inside when its leaf is a string, nothing otherwise —
and the comma that ends the member. Its leaf is written by the type the schema states
(`crate::leaf`), so `ctx_limit` takes `"70%"` and
`keep_alive` takes `true` even though both lists are published as plain texts. A
`model_ref` leaf is the object SPEC spells `Model := {provider, model}`, split out
of the roster's `<provider>/<model>`. A field the object already states is not
offered again.

**Measured, both ways.** `new_session{"live_compact_enabled":true}` with the caret
inside the name used to accept `id` and come out `new_session{"id":"",:true}` -- a
member written over one the line already had. It now offers nothing, and the line is left
as the operator wrote it. `/set_session{` accepts `id` and comes out `/set_session{"id":"",`:
the member's own comma is already at its tail, so the caret continues after a separator the
popup wrote rather than one the operator has to remember.

**The separator belongs to the member it ends.** A name writes the comma that closes it, so
the member after it needs no leading one; a value takes that comma over when it fills the
member (`comma`). The consequence is stated rather than hidden: a comma typed after an
accepted member leaves `{"id":"",,`, which no longer reads as a document, so the popup
offers names nowhere there.

**The braces an unfinished object owes are the operator's.** A candidate completes a
member, not a document: after `/cmd{` and one acceptance the line is `{"field":value`, and
the closers are keys to press. Writing them was what closed an object early and left the
operator's next member outside it.

**What a leaf with no list offers.** A string leaf offers the operator's own
fragment, closed, with the caret between the quotes while it is empty. A switch
offers `true` and `false`. Either is a *finished* token rather than a choice, so it
is offered only where the catalog publishes nothing and not while a listing is on
its way (`Catalog::wants`).

**Which model a value belongs to.** `accepts` is keyed by model, and a config
states its own: a value inside `model_config` reads the `model` its enclosing
object already carries before the line's model or the server's default is asked.

**Sticky.** The JSON form fills sticky entries by the same rule as the flag form
(`window_input::sticky_additions`).

## Where the shape comes from

`protocol::argschema::of(&spec.args)` turns a command's arguments into one JSON
Schema document — `type`, `properties`, `required`, `enum`, `description`,
`default` — and the completer reads only that.

**What the standard cannot state rides on one extension**, `x-completion`: the live
source a value is completed from, the value sets keyed by model, the placeholder
and the model-reference flag. The standard says an unknown keyword is an
annotation, so any other reader sees a plain schema. The source is carried as the
enum serde states it, so reading it back is the same pair that wrote it and there
is no second name table to keep in step.

**One table, one reading.** `protocol::argtype` is what spells SPEC's types as
JSON Schema types for the model's tool schema, and this document is built from the
same table, so a type is read once.

**A schema chosen by a sibling.** `x-completion.schema_of` names another property
and `x-completion.schemas` holds the document each of its values chooses, so
`run_skill{"id":"alpha","cmd":{` completes alpha's own arguments. The sibling is
read from the level the property lives in (`Place::levels`), and a line that has
not stated it yet completes nothing rather than guessing. It is what `oneOf`
cannot say without spelling every choice twice.

**One document, two readers.** The completion inside an object and the fill that
stands a sticky entry in for the leaves a line left open read the same document —
`argschema::properties`, `kind`, `default`, `source`, `accepts`, `model_ref`. The
sticky kind rides in the extension, so a value that belongs to a type is found the
same way by both.

**The skill producer.** Each package's publisher generates `schema.json` beside
`__init__.py` — the functions the package publishes, each with the schema its arguments
are — and the catalog reads that file (`content_commands::package_schema`) and hangs it
on `run_skill`'s `cmd`, keyed by skill id. So `cmd.name` enumerates that skill's
functions and `cmd.args` states the parameters of the chosen one, and no second list of
arguments can drift: nothing here reads a signature.

**Open**: `CommandSpec` carries no schema on the wire — the client builds it from
the published arguments with this one builder.
