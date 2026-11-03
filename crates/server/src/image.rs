//! Pictures: formats, media types, and what a provider will accept.

pub(crate) mod pictures;

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
    /// The copy this home holds, named by the id the message appends.
    ///
    /// # Invariant
    ///
    /// The bytes shown to a model come from this copy, never from the path: a
    /// picture a user has since overwritten is still the picture that was sent.
    pub copy: Option<i64>,
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
            copy: None,
            file_id: None,
        }
    }

    /// The pair a stored row keeps: a path and its type, never the bytes.
    pub fn to_json(&self) -> Value {
        let mut row = json!({"path": self.path, "media_type": self.media_type});
        if let Some(copy) = self.copy {
            row["copy"] = json!(copy);
        }
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
            copy: value.get("copy").and_then(serde_json::Value::as_i64),
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
/// The id beats the bytes, and the transcript's copy beats the path; a picture
/// nothing can read degrades to a note, never to a failed turn.
pub fn content_part(image: &ImageRef) -> Value {
    if let Some(id) = &image.file_id {
        return json!({"type": "input_image", "file_id": id});
    }
    // The copy first: a path says where a picture came from, not what was sent.
    let kept = image
        .copy
        .and_then(crate::image::pictures::read)
        .or_else(|| std::fs::read(&image.path).ok());
    match kept {
        Some(bytes) => json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.media_type, crate::base64::encode(&bytes)),
        }),
        None => json!({
            "type": "input_text",
            "text": format!(
                "[the image at {} is no longer readable and this home holds no copy, so \
                 whatever it showed cannot be recovered]",
                image.path
            ),
        }),
    }
}
