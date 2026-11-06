# server/tool_cmd.rs

## tool_cmd.rs

The tool commands: every tool the model can call is also a command an operator
can type, because a tool is a tunnel into a command, not a second surface.

## The command is the source of truth

SPEC's tool section says it in one sentence: a tool is a special cmd that agents
call directly, to pass arguments without escaping. The command catalog is the
definition; the tool is one address of it. So the catalog entry is generated
from the registry's own schemas -- the model's view and the operator's view of
`bash` are one definition, and a tool added to the registry is a command the
next greeting names. When scripts register commands dynamically, the registry
derives from the catalog instead and the two views stay one.

## Why the tunnel matters

An agent that wants to run a command used to have to spell it as a shell line
and worry about quoting -- every `"` and `$` in a message text is a character
someone must escape. The tool call carries the same arguments as structured
JSON, which the command layer binds by name: no escaping, no re-splitting, and
a value that means a number arrives a number. `coerce` types each value by the
tool's own schema, so the operator's line and the agent's call reach the same
implementation with the same shapes.

The schema here is `general_parameters`, not `Tool::parameters`: an operator can spell
the tunnel's arguments too, `--timeout_ms` and `--detached` included. Reading the raw
schema was right while the general arguments were a title and a cap; once they became
the tunnel's, refusing them on this path would mean the model may detach a call and the
operator may not, on one implementation with one schema.

## One runner

`tool_cmd::run` builds the `ToolContext` a command names and runs the tool.
Three callers share it: the cmd dispatch (`bash <session> <command>`), a
subscription's `--bash`/`--python` action, and -- through the same registry --
the model's own call inside a turn. Keeping one builder is what keeps the three
from drifting into three behaviors: the session's own shells and interpreters
are used everywhere, keyed {session, name}, so a command and the model meet in
the same persistent shell and a `cd` in one is seen by the other.

There is deliberately no streaming on the command path: `events` is `None` and
the whole outcome is the answer. A turn owns a stream; a command owns an
answer.

Explore mode rides this context as `explore`, from the session's own `SessionInner`, so
a command layer's `write_file` is refused on the same terms as a model's, and the look
that follows a foreground call is taken inside `ToolRegistry::run` -- once for every
caller -- rather than here: see `docs/notes/server-explore.md`.

## The tool list and the greeting

The catalog publishes every tool, including `remote_bash`, which only a session
with a remote can run: the tool itself refuses when there is no peer, which is
the same answer the model's call gets. Publishing anyway keeps one list -- the
greeting says what exists, the session decides what runs.

## Which arguments are positional

`positional_of` names, per tool, the arguments that spell bare on the line: the
object a tool acts on (`bash`'s command, `write_file`'s path and content, and
now `skill`'s name and argv), never the switches. The session is positional
everywhere because every tool command addresses one, and it is *hinted*
`<name>` while a tool's own `name` argument is hinted by its JSON type -- the
one collision this table cannot rename, since both are the caller's words.
