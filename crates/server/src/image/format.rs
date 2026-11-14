//! What a picture's bytes must be, by format.
//!
//! See `docs/notes/server-pictures.md`.

/// The media type these bytes are, when they are the whole of one.
///
/// # Invariant
///
/// Only the bytes answer, from a format's own signature to its own end marker: a
/// name saying `png` over bytes that stop after the signature is a picture every
/// provider refuses, and the request carrying it can never be sent again.
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(PNG_SIGNATURE) {
        return png(bytes).then_some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return jpeg(bytes).then_some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return gif(bytes).then_some("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|kind| kind == b"WEBP") {
        return webp(bytes).then_some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return bmp(bytes).then_some("image/bmp");
    }
    None
}

/// The eight bytes every PNG opens with; the rest of the file is chunks.
const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The header chunk is thirteen bytes and states nothing else.
const PNG_HEADER_LEN: usize = 13;

/// A chunk is a length, a type, its data and a checksum, all inside the file.
fn png_chunk(bytes: &[u8], at: usize) -> Option<([u8; 4], usize, usize)> {
    let head = bytes.get(at..at.checked_add(8)?)?;
    let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let kind = [head[4], head[5], head[6], head[7]];
    let next = at.checked_add(12)?.checked_add(length)?;
    (next <= bytes.len()).then_some((kind, length, next))
}

/// `IHDR` first, at least one `IDAT`, then `IEND` to close it.
fn png(bytes: &[u8]) -> bool {
    let mut at = PNG_SIGNATURE.len();
    let (mut header, mut data) = (false, false);
    let mut first = true;
    while let Some((kind, length, next)) = png_chunk(bytes, at) {
        // Every later chunk is read against the size the header states, so a file
        // that puts anything before it is not decodable.
        if first && &kind != b"IHDR" {
            return false;
        }
        first = false;
        if &kind == b"IHDR" {
            if length != PNG_HEADER_LEN {
                return false;
            }
            header = true;
        } else if &kind == b"IDAT" {
            data = true;
        } else if &kind == b"IEND" {
            return header && data;
        }
        at = next;
    }
    false
}

/// Opens with the start of image, states a scan, and closes with the end of image.
fn jpeg(bytes: &[u8]) -> bool {
    let scans = bytes
        .windows(2)
        .any(|pair| pair[0] == 0xFF && pair[1] == 0xDA);
    scans && bytes.ends_with(&[0xFF, 0xD9])
}

/// A trailer byte is the last thing a GIF writes; a header alone is a stub.
fn gif(bytes: &[u8]) -> bool {
    bytes.len() > 13 && bytes.ends_with(&[0x3B])
}

/// The RIFF length is the rest of the file, and the first chunk names the codec.
fn webp(bytes: &[u8]) -> bool {
    let Some(size) = le_u32(bytes, 4) else {
        return false;
    };
    let Some(kind) = bytes.get(12..16) else {
        return false;
    };
    (size as usize).checked_add(8) == Some(bytes.len())
        && (kind == b"VP8 " || kind == b"VP8L" || kind == b"VP8X")
}

/// The header sizes a bitmap may declare; anything else is another kind of file.
const BITMAP_HEADERS: &[u32] = &[12, 40, 52, 56, 64, 68, 108, 124];

/// The file's stated length, where its pixels start, and a header this format has.
fn bmp(bytes: &[u8]) -> bool {
    let (Some(size), Some(pixels), Some(header)) =
        (le_u32(bytes, 2), le_u32(bytes, 10), le_u32(bytes, 14))
    else {
        return false;
    };
    size as usize == bytes.len()
        && (pixels as usize) < bytes.len()
        && BITMAP_HEADERS.contains(&header)
}

/// The little-endian four bytes at `at`, when the file is that long.
fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}
