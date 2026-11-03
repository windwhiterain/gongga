# A description states its difference, not the whole tool again

User's instruction, 2026-11-03: `python` should mirror `bash`, and the schema should
not "repeat everything honestly" -- the remote and local tools should say "same as
`bash` except ..." so the tool list costs less on every request.

The tool list is one registry for the whole server (`Agent.tools`), so a local session
also pays for the peer's half. Measured before: `LOCAL_BASH_DESCRIPTION` 968,
`REMOTE_BASH_DESCRIPTION` 998, `DESCRIPTION` (python) 729, `REMOTE_DESCRIPTION` 696 --
3391 characters, every turn. After: 968 / 488 / 541 / 272 -- 2269, with `check_python`
added.

What a reference may point at: another *tool*, because the whole tool list is in the
same request. Never a note, and never prose the model would have to fetch.

## What this settled about background work

`python` and `remote_python` take `background` as `bash` does, `check_python` collects
it, and the completion notice is the shell's own function. The refusal is the point
the user asked for explicitly: **a foreground call to a name with a background call
running returns a name error**, because the driver pairs replies by order, so the
second reader would take the first answer.

The counter that decides it lives outside the interpreter's lock, and that is not
tidiness: a background call holds the lock for as long as the code runs, so reading
the machine through it made a busy name look idle. Measured as a foreground call
accepted, then answering 928ms later from the other request.
