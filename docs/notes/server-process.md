# server-process: what the OS says about the processes around this one

One function, `parent_of(pid)`, and two callers with different reasons to ask.

`tools::detached` asks it to tell a wrapped job from a detached one: a detached job
that turns out to be this server's parent must not be adopted, or `del_bash` would
kill the server it is running inside.

`supervise` asks it to check a claim: `GONGGA_SUPERVISED` in the environment is not
proof that this process is a server's child, because a shell inherits the
environment and passes it on. See `server-supervise.md`.

The module exists so the two do not each own a copy. Windows walks one ToolHelp
snapshot; unix asks `getppid` for itself and `ps -o ppid=` for anybody else, because
`/proc` is not shaped the same on macOS.
