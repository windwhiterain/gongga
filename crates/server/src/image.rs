//! Pictures: formats, media types, and what a provider will accept.

pub(crate) mod files;
pub(crate) mod pictures;
pub(crate) mod uploads;

/// What the provider answered when it took one picture: the file, and when it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub id: String,
    /// When the provider stops serving it, in millis since the epoch.
    pub expires_at: i64,
}

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
}

impl ImageRef {
    pub fn new(path: impl Into<String>, media_type: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            media_type: media_type.into(),
            copy: None,
        }
    }

    /// The pair a stored row keeps: a path and its type, never the bytes.
    pub fn to_json(&self) -> Value {
        let mut row = json!({"path": self.path, "media_type": self.media_type});
        if let Some(copy) = self.copy {
            row["copy"] = json!(copy);
        }
        row
    }

    /// Reads one back. A row written by an older build that carried `file_id` reads as
    /// the copy it also recorded: the id is forgotten, not trusted.
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            path: value.get("path")?.as_str()?.to_string(),
            media_type: value
                .get("media_type")
                .and_then(|m| m.as_str())
                .unwrap_or("application/octet-stream")
                .to_string(),
            copy: value.get("copy").and_then(serde_json::Value::as_i64),
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
