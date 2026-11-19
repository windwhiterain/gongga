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

## A row that only forwards is not published

A row whose `cmd` is its own name and names nothing this build implements hands its
call to the command layer (`Forward`), so its catalog entry would resolve back to
itself: one call, and no end. Such a tool is not published at all -- it is that
command's spelling, and the command is what the greeting carries. A row forwarding to
a name nothing answers is then an unknown command, which is the honest answer, instead
of an entry that loops.

That is also what makes a name one row: `ls_channel`'s server command was published
twice before this, once by `server_catalog` and once by this module.

## Why the tunnel matters

An agent that wants to run a command used to have to spell it as a shell line
and worry about quoting -- every `"` and `$` in a message text is a character
someone must escape. The tool call carries the same arguments as structured
JSON, which the command layer binds by name: no escaping, no re-splitting, and a value
that means a number arrives a number. Nothing types an argument on this path any more:
the wire carries values, so the operator line and the agent call reach the same
implementation with the same shapes -- the line converts its words through the argument
type it published (`protocol::argtype`), and both are held to the same kind.

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

## A forwarding row's arguments are the function's

A row whose `cmd` a package declares -- `[tool.read]` with `cmd = "read"` -- takes its
arguments from that function's signature. `ContentCommands::commands` already asks each
package what it publishes (`inspect.signature` through the contract) and publishes the
answer as a command; the same answer is written into `tools::Declared`, and
`Specd::spec` renders a forwarding row from it: the row keeps only what prose can say (a
sentence, a hint, a place on an operator's line) and a kind for an argument the function
left unannotated. One read of the content, so the model's schema, the check and the
operator's catalog cannot disagree.

Before that, the row was a second copy: `offset`/`limit` were offered as strings while
`read` was annotated `int`, `all` as a string while `edit` said `bool`, and `remote`/`cwd`
were missing from the row altogether, so a model could not name them. A row that restates
a kind the function stated is now ignored with a warning, and a name the function does not
take is dropped the same way.

## The tool list and the greeting

The catalog publishes every tool, including `remote_bash`, which only a session
with a remote can run: the tool itself refuses when there is no peer, which is
the same answer the model's call gets. Publishing anyway keeps one list -- the
greeting says what exists, the session decides what runs.

## Which arguments are positional

`positional_of` names, per tool, the arguments that spell bare on the line: the
object a tool acts on (`bash`'s command, `write_file`'s path and content),
never the switches. The session is positional
everywhere because every tool command addresses one, and it is *hinted*
`<name>` while a tool's own `name` argument is hinted by its JSON type -- the
one collision this table cannot rename, since both are the caller's words. It is
never *required*: SPEC gives it `caller()`, and the caller is exactly what a tool call
has, so a call that names only the tool's own arguments is complete.
