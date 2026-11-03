# `crates/server/src/image/pictures.rs`

SPEC's `## Meta` asks for three things about a picture a message names, and this file is
the second of them:

```
- `@image: <path>` , append id to it `@image: <path> (id)`.
  - copy the image to transcript, dedup by path.
```

`meta` appends the copy's id to the mention; `image::files` resolves that copy to the
provider's file when a request is written; the bytes are here.

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
does: the row keeps its path, the mention gets no id, and `image::files` reads the path,
which is what every build did before this file existed.

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
`read` answers `None` for it: the mention keeps its id, `image::files` falls back to the
path, and a request never carries a picture read from half of one.

## the provider's file is not recorded here

A row records the copy and never the file the provider minted for it. `uploads` (see
`uploads.rs`) holds `(scope, copy) -> (file_id, expires_at)`, and a request looks the id
up while it is being written.

The reason is that a provider deletes its files. DeepSeek keeps one for ever only if the
upload omits `expires_after` -- and permanent files are what exhaust an account: 10,000
files and 25 GiB, both counted per account, shared by every key on it. So `EXPIRES_AFTER_SECS`
is stated (seven days) and a file is reused only while `REFRESH_MARGIN_SECS` (an hour) of
life is left. A row that recorded the id would be a message that stops working when the
provider drops the file, and a transcript cannot be told to stop carrying it.

This is the one value `body.rs` resolves rather than copies, and it is not the kind of
rewrite that cost the prefix cache: a re-upload happens when a provider expires a file --
days apart -- while a re-rendered index happened at every step.

## uploads.rs

`scope` is the endpoint, never the API key: DeepSeek shares files across every key on one
account, so including the key would make an account's own uploads invisible to its other
keys. `reusable_at` is the margin rule. `oldest` is what a quota refusal gives up --
oldest-expiring first, because that file was about to be unusable anyway. `forget` is how
a refused id stops being an answer, and `forget_scope` is the hammer for a refusal that
did not name which file.

`expires_after` is stated rather than omitted. Omitting it makes the file permanent,
which sounds better and is worse: permanent files accumulate against the account's file
count and storage, and a file that never expires can only ever be deleted by hand.

## files.rs

`Files` is per request: it memoises what it resolved, so a transcript naming one picture
ten times uploads nothing twice. `part` returns the content part -- a file id when one can
be had, the bytes inline when it cannot, a note when neither. Inline is the fallback on
purpose: a picture that was sent yesterday must be sent today, and a missing Files API is
a bigger request, not a missing picture.

A quota refusal in `upload` deletes this endpoint's oldest files and tries once more. Once,
not in a loop: a second refusal means the account is actually full, and deleting more of
the user's pictures to learn that is not this code's business.

## a file the provider no longer has

`llm::stream` recognizes the refusal (`dead_file`) and, once per request, forgets this
endpoint's ids and rebuilds the body -- which uploads every picture again. It is not routed
through the retry counter: that counts a bad minute against a ceiling, and this is neither.

## measured

`.dev/pictures/probe.sh` runs one server against two providers -- one with a working Files
API, one whose uploads fail -- and checks, on the stored rows and the bodies the provider
received: the mention keeps `@image: shot.png (<id>)`, the copy holds exactly the named
bytes, a rewritten file mints a second copy while the first row keeps the first, a failed
upload still sends the picture out of the copy after the source file is deleted, and a
`@image:` printed by a command is copied the same way with the command's text left as
printed.
