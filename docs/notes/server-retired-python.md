# `crates/server/src/retired_python.rs`

## module doc

A build before `baa1dea2` wrote `gongga.py`, `gongga_cli.py` and `gongga_skill.py` into
`<home>/lib` on every start, from bytes compiled into the binary. The function that did it
went away; the files it had already written did not. The content repository now ships the same
three names through its `python` mapping, so a home whose `lib/` still holds the old copies is
in a half state that cannot resolve itself.

Why it cannot by itself: a copy and a reader's file are the same bytes on disk, and the
only honest test is whether they are *this build's*. `ship::tidy`, which takes away what an
older build installed, compares a file with the content's own copy -- but these three names
were a build's, not the content's, so their bytes are nowhere in the checkout to compare
against. `retire` therefore runs inside `tidy`, first, with the bytes an older build wrote
compiled into this binary.

## why whole bytes, not a hash

The names in the three offers match the content's, so the name says nothing about who wrote
the file. The bytes do. A `sha2` dependency would answer the same question, but it answers it
approximately -- a collision deletes a file the reader wrote -- while `include_bytes!` and a
`Vec<u8>` comparison answer it exactly, at the cost of 30 KB in the binary.

That cost is the three files under `src/retired/python/`. They are the bytes of a deleted
feature, kept for the only purpose they still serve: being the identity of what that feature
wrote. Nothing reads them, and nothing but `retire` may. `Retired` in `ship.rs` is a different
mechanism -- content-declared paths, matched by name -- and does not help here, because the
mapping that would have to name `lib/*.py` as retired is the content's, and the content wants
the path, not the old bytes.

## what is kept, and why that is the right answer

A file at one of these paths whose bytes differ is left exactly as it is. It can be the
reader's own module, or a newer content revision; either way it is not this build's artifact
and this build has no business deciding. `ship::tidy` reports it as kept, and the
one log line says which names were kept and why.

The whole step is one `read` per name on every start: no directory walk, nothing to remember
between starts, and nothing written unless a match is found.
