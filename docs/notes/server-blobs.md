# `crates/server/src/blobs.rs`

## module doc

Some provider items carry a payload -- `image_generation_call` returns a base64
picture -- and the two honest homes for those bytes are both wrong: megabytes of
base64 in a transcript row ride along in every later request forever, and dropping
them leaves an item that cannot be sent back, which turns "generate a picture and
keep working" into a session that silently loses its own history. So the bytes go
into a file beside the home and the row keeps the id they live under.

This is not the cache, and not the archive. It is the same thing the transcript is:
the record of what the model was shown. A row without its payload is a hole in that
record, and `sendable_activity` says so rather than filling it in.

## DIR, install, dir

A process global, because the code that has a payload in hand -- the SSE handler, in
the middle of a stream -- has no config and no home to ask. `install` is called once
at startup beside the output directory, and creating it there means a first picture
does not have to reason about whether it exists.

Without a home the module degrades loudly instead of guessing at one: the payload is
not written, the item stays non-replayable, and the warning names the type and the id.
That is what every build did before this file existed, so a test or a tool that never
installs a home is not silently losing data.

## file_name

The provider's item id is the name. It is escaped rather than filtered: every
character that could name a directory (`/`, `\\`, `:`) becomes its `%XX` and nothing
else is touched, which makes the map injective. That matters more than it looks -- a
lossy map sends two different ids to one file, and the second write would replace the
first session's picture with another's, unnoticed by both.

The name is built by appending an extension, not with `Path::with_extension`: for a
name like `..json` Rust's idea of a stem turns `..json` into something else, and the
temporary file lands where no reader looks.

## put

Written whole into a `.part` and renamed onto place. A reader that sees the file sees
all of it; a crashed write leaves a `.part` nobody reads and an item that still says
"not sendable", which is the same answer as before the write started.

The bytes stored are the value's own JSON text, not its string contents: whatever type
the provider chose -- a base64 string today, an object of parts tomorrow -- comes back
out identical, and the reassembled item is byte-equal to the one that arrived.

## drop_all, and who decides

The database decides reachability (`Store::orphaned_blobs`), this module only touches
files. That split is the whole design: a blob can be named by more than one session --
an `heir` copy put the rows in a mentor session, and both transcripts now point at the
same bytes -- and only a query over every row can tell an orphan from a shared file.
Asking before the rows are deleted is not a style preference: after them there is
nothing left to ask about.

## Cross-home caveat

A `result_blob` is an id, and the bytes live in the home that generated them. Copying a
row between homes -- an export, a `heir` into another server -- leaves the reference
pointing at a file that is not there, and the item then refuses to send with a warning
naming it. That is the correct failure; inventing a payload would not be.
