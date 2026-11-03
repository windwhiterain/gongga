# `crates/server/src/image/pictures.rs`

SPEC's `## Meta` asks for three things about a picture a message names, and this file is
the second of them:

```
- `@image: <path>` , append id to it `@image: <path> (id)`.
  - copy the image to transcript, dedup by path.
```

`meta` appends the id and `body` substitutes the File ID at send time; the copy is here.

## why a copy at all

A path is where a picture came from, not what was sent. A conversation that names
`shot.png` six times is a conversation about six screenshots, and the seventh read of the
path is a different picture from the one the model was shown. A transcript that pointed at
the path would silently rewrite what the model saw, which is the one thing a transcript
may not do -- and a deleted file would leave a row that says a picture was there and no
way to show it.

So the bytes are copied once, under an id the row records. The same argument as
`blobs.rs`, which keeps a provider's generated picture beside the home for the same
reason; the difference is that a blob is named by the provider's item id and a picture by
this table's row, because nothing outside has named it yet.

## where the copy lives

`<home>/pictures/<id>`, beside the database rather than in it: a row is read on every
request and a picture is a megabyte. `install` is called by `Store::open`, which is the
one place that knows where the database is -- `blobs` is installed by `lib.rs` because a
provider payload arrives in the middle of a stream, while a picture arrives at delivery,
with the store in hand. Without a home (`:memory:`) the module degrades the way `blobs`
does: the row keeps its path, the mention gets no id, and `image::content_part` reads the
path, which is what every build did before this file existed.

The file has no extension: the media type is in the row that names the id, and the id is
the whole name, so a picture can never be read as a different kind because a suffix went
missing.

## remember -- dedup by path, and why a rewrite is a new copy

SPEC says dedup by path, and that is the lookup: `store::picture_of` answers the newest
row for that path, and its copy is compared with the bytes just read. Equal bytes, equal
media type, same id -- a message naming the same picture again adds no bytes.

Unequal bytes are a new row and a new copy, never an overwrite. Overwriting would keep the
id stable and change what every earlier message shows, which is the same silent rewrite
the copy exists to prevent. The cost is one more file per rewritten path, and the newest
row is the only one a new message can reach.

A rewrite is compared against the newest row only, so a path that goes v1 -> v2 -> v1
again writes a second copy of v1's bytes rather than finding the first. That is a wasted
file, never a wrong picture, and finding the first would mean reading every copy of a path
on every delivery.

## the row and the file disagreeing

`remember` writes the row first, because the id is what the copy is named by, and drops
the row again if the write fails. A crash between the two leaves a row with no file, and
`read` answers `None` for it: the mention keeps its id, `content_part` falls back to the
path, and a request never carries a picture read from half of one.

## measured

`.dev/pictures/probe.sh` runs one server against two providers -- one with a working Files
API, one whose uploads fail -- and checks, on the stored rows and the bodies the provider
received: the mention keeps `@image: shot.png (<id>)`, the copy holds exactly the named
bytes, a rewritten file mints a second copy while the first row keeps the first, a failed
upload still sends the picture out of the copy after the source file is deleted, and a
`@image:` printed by a command is copied the same way with the command's text left as
printed.
