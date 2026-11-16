# cli/complete.rs

The completion: the vocabulary a front-end is answered in, and the one place the
line's JSON is read.

## complete.rs

`Item` is one option: `label`, `detail`, `new_text`, `snippet`, and `range` when
its token is not the answer's own. `Offered` is one answer: `subject` (what the
popup says it is asking for), the range every item replaces, and the items.
`Completion` is the open popup: the line and caret it answers, the same range,
the items, and the selection.

**The item writes a whole token and says where the caret goes.** `new_text` is
everything the line holds after accepting it — the colon, the quotes, the brace
it opens, the comma it owes, the closers its object does — and the caret lands on
the snippet stop `$0`. Nothing else computes an offset: `Candidate::cursor`,
`Candidate::space` and the per-candidate caret arithmetic are gone, so a new kind
of option cannot disagree with the front-end about where the caret is.

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

**A key place replaces the colon that follows it**, including one the line
already holds: otherwise accepting a name leaves the operator's colon after the
one just written.

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

**What it inserts.** A name is `"name":`, plus `{}` when the field holds an object,
plus `""` with the caret inside when its leaf is a string. A leaf value is written
by the type the schema states (`crate::leaf`), so `ctx_limit` takes `"70%"` and
`keep_alive` takes `true` even though both lists are published as plain texts. A
`model_ref` leaf is the object SPEC spells `Model := {provider, model}`, split out
of the roster's `<provider>/<model>`. A field the object already states is not
offered again.

**The separator is the line's answer.** A comma stands after the member when the
next non-whitespace character is not `,` `}` `]` and the line does not end there —
the standard rule. It is read off the line, never off the schema, so no trailing
comma is written and nothing already typed after the caret is touched.

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

**One document, two readers.** The completion inside an object and the fill that
stands a sticky entry in for the leaves a line left open read the same document —
`argschema::properties`, `kind`, `default`, `source`, `accepts`, `model_ref`. The
sticky kind rides in the extension, so a value that belongs to a type is found the
same way by both.

**Open**: a skill's `Cmd` schema has no producer yet; when one publishes, it feeds
the same reader. `CommandSpec` carries no schema on the wire — the client builds it
from the published arguments with this one builder.
