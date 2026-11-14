# `crates/server/src/retired_python.rs`

## module doc

A build before `baa1dea2` wrote `gongga.py`, `gongga_cli.py` and `gongga_skill.py` into
`<home>/lib` on every start, from bytes compiled into the binary. The function that did it
went away; the files it had already written did not. The content repository now ships the same
three names through its `python` mapping, so a home whose `lib/` still holds the old copies is
in a half state that cannot resolve itself.

Why it cannot: `ship::reconcile` treats a file with no entry in `.ship-base/` as the reader's
own, and keeps it (`Decision::Keep`). That rule is right -- it is what stops an install from
overwriting a module somebody edited -- and it is exactly why a build artifact masquerades as
one forever. Where the shipped bytes had not moved the two happen to be equal and the file is
adopted silently; where they had not moved, the home keeps its old copy and reports it as the
reader's on every start -- which is the state `~/.gongga/lib` was in on 2026-11-14, with
`gongga_skill.py` differing and the other two equal.

So the old files are retired before the content lands: `lib.rs` calls `retire` after `migrate`
and before `ship::install`, which is the only window where the paths still hold nothing but
what a build wrote.

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
and this build has no business deciding. The content's reconcile reports it as kept, and the
one log line says which names were kept and why.

The whole step is one `read` per name on every start: no directory walk, nothing to remember
between starts, and nothing written unless a match is found.
