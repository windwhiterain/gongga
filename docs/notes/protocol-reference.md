# `crates/protocol/src/reference.rs`

One list of three spellings, in the crate both sides already share.

The server parses `@image(...)`, `@skill(...)` and `@file(...)` in `crates/server/src/meta.rs`;
the client completes them in `crates/client/src/catalog.rs`. Two lists would let a
completed reference be one the parser does not read, and the failure would look
like a completion that inserts nothing.

What stays out of here is what each kind *means*: the parser resolves a picture,
the popup says "attach a picture". Neither belongs in a wire type.

## `meta_prefix`: the machine inside the group

`remote://` is the one spelling that names a machine, and it sits inside the reference's
own group: `@file(remote://p/notes.md)`. It lives here for the same reason the three
prefixes do -- the server parses it and the client completes it, so one spelling is what
keeps a machine a caller picked parseable, and a value with one group stays one value.
