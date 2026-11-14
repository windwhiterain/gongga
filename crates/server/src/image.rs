//! Pictures: formats, media types, and what a provider will accept.

pub(crate) mod files;
pub(crate) mod format;
pub(crate) mod pictures;
pub(crate) mod uploads;

/// The media type of a file, from what its bytes are.
///
/// # Invariant
///
/// The bytes decide, never the extension a file happens to carry, and they must be
/// the whole of what they claim: a provider refuses a picture it cannot decode, and
/// that refusal is a request which can never be sent again. See
/// `docs/notes/server-pictures.md`.
pub use format::media_type;

/// What the provider answered when it took one picture: the file, and when it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub id: String,
    /// When the provider stops serving it, in millis since the epoch.
    pub expires_at: i64,
}

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

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

    /// Reads one back. A row from an older build that carried `file_id` reads as
    /// the copy: the id is forgotten, not trusted.
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
