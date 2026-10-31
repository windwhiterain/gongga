# `crates/protocol/src/reference.rs`

One list of three spellings, in the crate both sides already share.

The server parses `@image:`, `@skill:` and `@file:` in `crates/server/src/meta.rs`;
the client completes them in `crates/cli/src/catalog.rs`. Two lists would let a
completed reference be one the parser does not read, and the failure would look
like a completion that inserts nothing.

What stays out of here is what each kind *means*: the parser resolves a picture,
the popup says "attach a picture". Neither belongs in a wire type.

## tests::every_prefix_names_one_kind

The three prefixes do not shadow each other, and a prefix without its colon --
`@image` -- is prose, not a typo to guess at.
