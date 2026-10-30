# cli/catalog.rs

## catalog.rs

The TUI supplies the token; this decides what could go there. There is one
catalog, published at connect time, and this client keeps no list of its own:
every command completes from the specs the greeting carried, including the
client's own ones and whatever a script registered.

What the client does own is the *state* those specs point at. `--name` for a
sticky argument completes from the sticky arguments that exist, and `--name` for
an event filter from the filters that exist: the server names the source
(`sticky_arguments`, `event_filters`) and the side that owns it fills it in, the
same way `sessions` and `paths` already worked.

Values complete from whatever the spec says can be enumerated -- the live
session list, a fixed set, every argument name, the filesystem, client state, or
another argument's list when the value is a name that chooses one.

## Inner::cwd

The client's own directory, because that is the one the user was looking at when
they typed.

## Catalog::set_cwd

Production never moves it: the user's shell is already in the directory they
mean. Tests need to aim it at a tree they built.

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

## Catalog::complete — flag values

Past the command name: only a flag's value, and only when the spec says where
the candidates come from. The flag sits one word back when the value has not
been started yet, and two when it has: `--session <TAB>` and `--session sc<TAB>`
both complete the same list.

## resolve

`depth` exists for the single case that points back into the catalog: a value
completed the way another argument's values are. One hop is enough for the shape
the client actually declares; the second is allowed only so a chain that names
itself is cut off rather than followed forever.

## flags_on_line

Both spellings count, `--name value` and `--name=value`. `skip` is the word the
cursor is in: it is the one being completed, so it is not yet a flag the line
has.

## paths

Directories come back with a trailing `/` and without the usual trailing space,
so Tab twice walks down a tree instead of starting a new word.

## directory

Read relative to the client's directory; `~` is expanded because `--cwd ~/work`
is how people write it.

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

A name can mean one thing in two commands (`set_session --session` targets a
session; `show_session --session` names one to draw), and the hop prefers the
argument that is *required*: that is the reading a free-form value should mirror.
Catalog order does not decide it.

## the_flag_being_typed_is_still_a_candidate

The word being completed is not "already filled": typing `--sess` must still
offer `--session`, or the flag could never be completed at all.

## a_finished_command_does_not_reopen_the_command_list

A space after the command name is the start of its flags, not another
opportunity to pick a command: reopening the whole list there covers the line
the operator has already decided.

## a_sticky_value_completes_like_the_argument_it_fills

The point of the sticky value: it fills `session`, so it should offer sessions
-- without the user having to remember the names.
