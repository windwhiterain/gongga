# cli/catalog.rs

## catalog.rs

The TUI supplies the token; this decides what could go there. There is one
catalog, and the server publishes it at connect time: every command completes from
the specs the greeting carried, whatever a script registered included.

The one exception is the client's own commands -- `connect_server`, `set_render`,
`set_render_session`, `set_sticky`, `del_sticky`, `serve`. They come from the
published spec SPEC gives them (`protocol::client_commands`, the same list `serve`
parses against) and are in the catalog from the first frame, because a window with
no server is still a window: the operator types them, completes them and has them
run while the link is down. For a name both lists carry, the server's spec is the
one the line is parsed with.

# Invariant

A command the *server* runs still needs a greeting: with no link there is no spec
for it, and `submit_tokens` says "not connected" instead of "unknown command".

What the client does own is the *state* those specs point at. `--name` for a
sticky argument completes from the sticky arguments that exist, and `--name` for
an event filter from the filters that exist: the server names the source
(`sticky_arguments`, `event_filters`) and the side that owns it fills it in, the
same way `sessions` and `paths` already worked.

Values complete from whatever the spec says can be enumerated -- the live
session list, a fixed set, the argument types the catalog declares, the
filesystem, client state, another argument's list when the value is a type that
chooses one, or the greeting's model table when the value is about a model.

## Inner::sessions, message_session, server_cwd

Three fields, one question: *which directory does this line read?* The answer is
the addressed session's own, and the other two are the cases where there is no
session to ask.

`SessionInfo` carries `cwd` and `target`, so the catalog keeps the sessions as
published rather than as names plus a side map. `message_session` is the sticky
`session` value: a message is addressed by it, and a message is where an
`@skill:` reference is typed. `server_cwd` is the directory the server runs in,
which is where a session with no directory of its own starts.

The client's own process directory is deliberately *not* the fallback any more.
It used to be, and it is where the bug came from: a window launched from `C:/`
offered `C:/.agents/skills` for a session working in a checkout, while the
server's own `index_skill` read the checkout's. Two directories that agree only
by luck is not a contract. `server_cwd` is the datum that makes them agree, and
it arrives in the greeting's `ServerInfo`, which already existed.

## place

Resolved once per completion, before the popup looks at any value: the machine
`--remote` names, then the machine the line's session runs on, then this one --
and the directory, which is the session's when the line has one. The session a
line names is the value bound to its kind-`session` argument, wherever the spec
keeps that argument and however it is spelled, and `session_named` reads it
through `parse_args` -- the one binder, so completion and submission can never
disagree about what a line says. A message names no session on the line, which is
why `words` is empty for one: prose containing `--session` is prose, and the
session it goes to is the sticky one.

## remote_paths, wanted, inflight, fail_paths

A peer's listing cannot be read synchronously, so the completion offers what has
arrived and asks for the rest: `wanted` holds the one directory each remote is
still missing, `take_wanted` moves it to `inflight`, and the answer lands in
`remote_paths` keyed by the pair it answered.

`inflight` is what stops the same question being asked on every keystroke, which
makes releasing it the whole of a refusal: an unanswered listing that stayed in
flight would never be asked again, and its popup would stay empty for good. A
refused or unanswered `list_paths` therefore calls `fail_paths`, and the next
completion asks again.

One entry per remote rather than one per directory is deliberate: a queue keyed by
directory asks for a listing per letter typed, answers them in order, and is always
behind the cursor.

## skill_names

Computed lazily against `SkillKey` -- the session's directory and the home --
rather than pushed in from a setter. The old shape was a `set_skill_names` call
from `sync_sticky`, which meant the list was right only at the call sites that
remembered to run it; a session that moved, or a window that switched its sticky
session, left a list that no longer described anything. A cache keyed by what the
answer depends on cannot go stale without being asked again.

## Ctx

Snapshotted out of the catalog so the lock is not held while the filesystem is
read: a directory read that blocks would otherwise hold the write lock out too,
for as long as the disk takes.

## Catalog::complete

One grammar, decided by the first character: `/` starts a command, and anything
else is a message to the agent. Completion is about commands, so a message --
free text, in any language, with whatever punctuation the operator likes --
offers nothing at all.

That is also what keeps the popup out of the way: without this rule, every
keystroke of an ordinary sentence matched some command name and opened a list
over half the screen, including while clearing the line.

## Catalog::complete — word_at_cursor

The word the cursor is in, when it is in one: that word is about to be replaced,
so it does not count as already on the line. Without this, typing `--sess` would
see a flag it considers filled and offer nothing.

## Catalog::complete — flag names

A flag already on the line is not a candidate: the command would take the second
value for the same argument (or refuse it), and listing it again hides the flags
still missing.

## Catalog::complete — command names

Command names, while the command has not been written yet. A space *after* it is
not a command name any more: offering the whole list there reopened the popup
over a line whose first word was settled.

## Catalog::complete — values

Past the command name, a word completes one of two things. A flag's value, when
it follows a flag: the flag sits one word back when the value has not been
started yet, and two when it has: `--remote <TAB>` and `--remote lo<TAB>`
complete the same list. Or the next orderable slot -- a positional or a required
argument, skipping ones a flag on the line already bound -- counted from the bare
words on the line (`positional_tokens` steps over flags and their values):
`/set_session <TAB>` offers sessions, and with a session on the line the next Tab
offers models. A slot whose source is `None` offers nothing, which is how
`send_message --name <name> --message <text>` stops completing after the session.

## models, default_model, and the two sources that read them

`Catalog::set_models` keeps the greeting's table as published, and `default_model` holds
the model the server itself falls back to -- both set on every greeting, and the table
again whenever the server republishes it (`Event::Models`, sent when `providers.toml` is
edited). So a window that stays open completes from the roster as it is now, not as it
was when the window connected. A tuning value is worth nothing on its own:
`--thinking high` means one thing on a model that publishes three levels and another on
one that publishes five, so the value's list has to ask which model the line runs on.

`model_named` answers that in the order the command itself resolves a model: the value
bound to the line's kind-`model` argument, else the model the session the line names
already has, else the server's default. The middle step reads `SessionInfo::model`, and
the session is the one `session_named` finds -- the same binder `place` uses, so a value
that belongs to a session is read the same way wherever it is read. `level_choices` then
reads that model's levels, or -- when no model came out of it, one the roster does not
describe came out, or that model publishes nothing -- every level some model here
publishes. The union is not a guess at a default: such a model takes whatever it is
sent, so nothing in the union can be refused, and a popup that offered nothing would be
this client inventing a restriction the roster never stated. `THINKING_DEFAULT` goes on
the end of either list.

## resolve

`depth` exists for the single case that points back into the catalog: a value
completed the way another argument's values are. The type the line's first word
names -- `set_sticky <type> <value>` completes the value as arguments of that
type complete, `session` offering sessions. One hop is enough for the shape the
client actually declares; the second is allowed only so a chain that names itself
is cut off rather than followed forever.

## flags_on_line

Both spellings count, `--name value` and `--name=value`. `skip` is the word the
cursor is in: it is the one being completed, so it is not yet a flag the line
has.

## pick and the command list — a match is anywhere in the word

SPEC asks for candidates "in any char position", so `pick` tests `contains` rather
than `starts_with`. An exact prefix match sorts first (a stable sort, so the
catalog's own order decides between equals): Tab on a word that is already whole
completes nothing instead of reordering the popup under the operator's eyes.

The command list is the same rule, and it was the one place still prefix-only:
`/render` found nothing, because no command starts with "render" -- `set_render` and
`set_render_session` have it in the middle. `a_command_name_matches_anywhere` is the
test that would have caught it.

## reference — a message's `@` references

A line that does not start with `/` is a message, and the only thing worth
completing in one is an `@` reference: `@image:`, `@skill:`, `@file:` first, then
the value each one names -- a path for a picture or a file, a skill's name for a
skill. Two steps, because the prefix is a decision and the value is a lookup.

## paths

Directories come back with a trailing `/` and without the usual trailing space,
so Tab twice walks down a tree instead of starting a new word.

## directory

Read relative to the base `place` chose -- the session's own directory, or the
server's -- never relative to this process; `~` is expanded because
`--cwd ~/work` is how people write it.

## client_specs

Their arguments complete against the sticky arguments that exist right now, so
the specs are rebuilt on change rather than patched.

Two states feed it, and both rebuild the whole list: the sticky arguments for
`del_persistant_cmd_argument --name`, and the event filters for
`del_event_filter --name`. Keeping one setter would make each change erase the
other's candidates, so `set_sticky_names` and `set_filter_names` each write only
their own field and re-derive from both.

`set_event_filter --name` offers the wire's field vocabulary instead of the
catalog's argument names, because it names an *event* field. Its `--value`
completes as the argument of the same name would -- `session` offers sessions --
through the same `OfNamedArgument` hop the sticky value uses.

A name can mean one thing in two commands (`set_session <session>` targets a
session; `show_session <session>` names one to draw), and the hop prefers the
argument that is *required*: that is the reading a free-form value should mirror.
Catalog order does not decide it.
