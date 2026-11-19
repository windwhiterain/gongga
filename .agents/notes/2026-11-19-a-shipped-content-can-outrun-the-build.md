# A shipped content can outrun the build that reads it

## What happened

Since 2026-11-17 the image skill prints `@image(remote://<peer>/<path>)`: content
`051740e`, whose commit message says so in as many words. The harness half -- the
`remote://` spelling in `protocol`, the reader in `meta`, the completion in the
client -- sat unmerged on `feature/space-unified`, and `git log -S'remote://' main`
answered nothing at all.

So every remote-qualified picture was dropped in silence. `meta` read `remote://p/x`
as a path, the read failed, `picture` returned `None`, and the mention stayed in the
transcript as the literal text: no `pictures` row, no `uploads` row, no log line.

## The evidence it was silent

- `pictures` held no row containing `remote://`, and `uploads` names no copy of one.
- `Rusd/mentor/4` (352) seq 411: the skill printed
  `@image(remote://lichen-vm/C:/Users/17124/rusd-client/tier-sample.png)` for files
  that were on the server, and the model saw nothing. It then printed bare
  `@image(C:/...)` from an `exec` and wrote "I can see the three renders" (seq 420).
- `Rusd/mentor/3` (338) seq 1173 reaches the same conclusion in prose.
- A local reference was never the problem: the paste in session `fix` (copy 305)
  uploaded, and a direct `/v1/responses` request naming that `file_id` read the
  picture's first line back.

## Why no gate saw it

A spelling the reader must parse is a change of shape, so `protocol` had to move
first, then the harness, then the content. Both sides stayed at 13, so
`scripts/ship.sh --check` was green the whole time: it compares protocol numbers,
and the number never moved.

## Merged 2026-11-19

`d5645651` carries the four commits as one. Conflicts resolved without reverting
main: the ssh probe keeps `-- "bash -lc true"`, the client's skill arm keeps
`narrowed`, and `list_skills` moved to `installed_in` + `found_row` (the walk was
renamed under it). `scripts/remote_meta_selftest.sh` PASS 26s, eight checks,
against the operator's peer, including that a peer's picture is read, copied into
this home and sent as an image part.

## Still open, in the same shape

`feature/mentor-attribute` (one commit) is what raises `protocol` to 14, and the
content already speaks it: `ship.sh --check` is red (content `c01a144` = 14, this
build = 13). `evolve stage` refuses on that check, so a home cannot take a new
build until that branch is merged too.

## Rules that follow

- A content change that adds a spelling or a shape the server must parse raises
  `protocol` first, ships the harness, and only then publishes the content.
- A reference that cannot be resolved should leave a mark rather than vanish; the
  reader in `meta` says why it gave up since `e2829dfe`, but a build that does not
  parse the spelling never gets that far.
