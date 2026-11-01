# `crates/server/src/base64.rs`

The runner channel is text, and a file's bytes are not. This is the whole of the
encoding it needs, so the wire costs no dependency.

`decode` refuses anything outside the alphabet rather than skipping it: a decoder
that guesses turns one corrupt frame into plausible wrong bytes, and the caller
would store those. `encode`/`decode` round-trip every byte value, which is what a
`read` reply of a binary file relies on.
