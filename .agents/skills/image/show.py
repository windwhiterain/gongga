#!/usr/bin/env python3
"""Print a picture so the harness attaches it to this turn.

The harness scans a bash result for `@<path>.<image ext>` and uploads each one to
the provider's Files API, so the model receives an `input_image` part instead of
having to decode base64 in its head. This script's whole job is to print exactly
that reference, once, for a path that really is a picture.

    show.py <path>...        # print @-references for every readable picture
    show.py --check <path>   # say what it is, print nothing
"""

import os
import sys

# One list, mirrored from crates/server/src/image.rs. The harness accepts these
# extensions; anything else is printed as a note, never as a reference.
KNOWN = {
    "png": "image/png",
    "jpg": "image/jpeg",
    "jpeg": "image/jpeg",
    "gif": "image/gif",
    "webp": "image/webp",
    "bmp": "image/bmp",
}

MAGIC = [
    (b"\x89PNG", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF8", "image/gif"),
    (b"BM", "image/bmp"),
]


def media_type(path, head):
    """The picture type, from the magic bytes, agreeing with the extension.

    The extension alone is not enough: a truncated or empty file named .png used to
    pass, and the reference then failed at upload instead of here. The header is the
    authority; the extension only has to be one the harness accepts.
    """
    ext = os.path.splitext(path)[1].lstrip(".").lower()
    by_magic = None
    for magic, media in MAGIC:
        if head.startswith(magic):
            by_magic = media
            break
    if by_magic is None and head[:4] == b"RIFF" and head[8:12] == b"WEBP":
        by_magic = "image/webp"
    if by_magic is None:
        return None
    if ext in KNOWN:
        return by_magic
    return None


def main(argv):
    check = "--check" in argv
    paths = [a for a in argv if a != "--check"]
    if not paths:
        print("usage: show.py [--check] <path>...", file=sys.stderr)
        return 2

    printed = 0
    for given in paths:
        path = given if os.path.isabs(given) else os.path.join(os.getcwd(), given)
        try:
            with open(path, "rb") as f:
                head = f.read(16)
            size = os.path.getsize(path)
        except OSError as e:
            print(f"cannot read {given}: {e}", file=sys.stderr)
            continue
        if size == 0:
            print(f"cannot read {given}: the file is empty", file=sys.stderr)
            continue
        media = media_type(path, head)
        if media is None:
            known = ", ".join(KNOWN)
            print(
                f"{given} is not a picture this harness takes ({known}); its header "
                f"is not one of them (first bytes: {head[:8]!r}). Convert it first",
                file=sys.stderr,
            )
            continue
        if check:
            print(f"{given}: {media}, {size} bytes")
            continue
        # The reference must be its own token: the harness splits on whitespace.
        print(f"@{path}")
        printed += 1

    return 0 if printed or check else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
