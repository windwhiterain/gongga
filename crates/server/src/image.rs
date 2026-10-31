//! Pictures: formats, media types, and what a provider will accept.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The picture formats this harness accepts, by extension.
pub const KNOWN: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
];

/// Where a picture is, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub path: String,
    pub media_type: String,
    /// A provider-side id, once one has been minted for this file.
    ///
    /// # Invariant
    ///
    /// The recorded request carries the id, not the bytes: a transcript replayed
    /// ten times must not carry ten copies of a screenshot.
    pub file_id: Option<String>,
}

impl ImageRef {
    pub fn new(path: impl Into<String>, media_type: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            media_type: media_type.into(),
            file_id: None,
        }
    }

    /// The pair a stored row keeps: a path and its type, never the bytes.
    pub fn to_json(&self) -> Value {
        let mut row = json!({"path": self.path, "media_type": self.media_type});
        if let Some(id) = &self.file_id {
            row["file_id"] = json!(id);
        }
        row
    }

    /// Reads one back, tolerating a row written before `file_id` existed.
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            path: value.get("path")?.as_str()?.to_string(),
            media_type: value
                .get("media_type")
                .and_then(|m| m.as_str())
                .unwrap_or("application/octet-stream")
                .to_string(),
            file_id: value
                .get("file_id")
                .and_then(|i| i.as_str())
                .map(str::to_string),
        })
    }
}

/// `given` resolved against `cwd` when it is relative.
pub fn resolve(cwd: &Path, given: &str) -> PathBuf {
    let path = PathBuf::from(given);
    if path.is_absolute() || cwd.as_os_str().is_empty() {
        return path;
    }
    cwd.join(path)
}

/// The media type of a file, from the extension first and the magic number second.
///
/// # Invariant
///
/// A picture is only offered when its kind is certain: a wrong media type is a
/// request the provider rejects, and the model cannot tell why.
pub fn media_type(path: &Path, bytes: &[u8]) -> Option<&'static str> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if let Some((_, media)) = KNOWN.iter().find(|(e, _)| *e == ext) {
            return Some(media);
        }
    }
    // No usable extension: the first bytes still say what it is.
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}

/// One picture as a request content part.
///
/// # Invariant
///
/// A file id is preferred to the bytes, and a file that is gone degrades to a note
/// rather than failing the turn: the transcript says the picture was there.
pub fn content_part(image: &ImageRef) -> Value {
    if let Some(id) = &image.file_id {
        return json!({"type": "input_image", "file_id": id});
    }
    match std::fs::read(&image.path) {
        Ok(bytes) => json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.media_type, base64(&bytes)),
        }),
        Err(e) => json!({
            "type": "input_text",
            "text": format!(
                "[the image at {} is no longer readable ({e}); it was here when this \
                 tool call ran, and whatever it showed cannot be recovered]",
                image.path
            ),
        }),
    }
}

/// Minimal base64, so the harness carries no dependency for one encoding.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_id_is_preferred_to_the_bytes() {
        let mut image = ImageRef::new("/does/not/exist.png", "image/png");
        image.file_id = Some("file-api-1".into());
        let part = content_part(&image);
        assert_eq!(part["file_id"], "file-api-1");
        assert!(
            part.get("image_url").is_none(),
            "no bytes when an id exists"
        );
    }

    #[test]
    fn a_picture_that_is_gone_says_so_instead_of_failing() {
        let part = content_part(&ImageRef::new("/gone/x.png", "image/png"));
        assert_eq!(part["type"], "input_text");
        assert!(part["text"].as_str().unwrap().contains("/gone/x.png"));
    }

    #[test]
    fn a_row_round_trips_with_and_without_an_id() {
        let plain = ImageRef::new("a.png", "image/png");
        assert_eq!(ImageRef::from_json(&plain.to_json()), Some(plain));
        let mut minted = ImageRef::new("b.png", "image/png");
        minted.file_id = Some("file-api-2".into());
        assert_eq!(ImageRef::from_json(&minted.to_json()), Some(minted));
    }

    #[test]
    fn base64_matches_a_known_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
