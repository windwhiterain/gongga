//! `read_image`: hand a picture to the model, by path.
//!
//! See `docs/notes/server-tools-read-image.md`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};

use crate::store::ImageRef;
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// The picture formats this tool recognises, by extension.
const KNOWN: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
];

pub struct ReadImageTool;

impl Tool for ReadImageTool {
    fn name(&self) -> &'static str {
        "read_image"
    }

    fn description(&self) -> &'static str {
        "Look at a picture on this machine: a screenshot, a chart, a diagram, a \
         photo. The image itself is handed to you, so you can read what is in it \
         -- this is not a text tool, do not use `bash` plus base64 for this. A \
         relative `path` is resolved against the session's working directory. \
         The picture is referenced by path in the transcript, so the file has to \
         still be there when the conversation is replayed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file. Relative paths are \
                                    resolved against the session's working directory."
                },
                "task": {
                    "type": "string",
                    "description": "What you are looking for, in a few words. Optional, \
                                    and never executed: it only labels the call."
                }
            },
            "required": ["path"]
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(given) = args["path"].as_str().filter(|p| !p.is_empty()) else {
                return ToolOutcome::err("read_image requires a non-empty string `path`");
            };
            match look(&ctx.cwd, given, &ctx.vision) {
                Ok((image, text)) => ToolOutcome::ok(text).with_image(image.path, image.media_type),
                Err(message) => ToolOutcome::err(message),
            }
        })
    }
}

/// The whole of `read_image` except the plumbing: a picture, or why not.
///
/// # Invariant
///
/// The roster is asked before the file is read: refusing on the model's own
/// stated modalities is the only refusal that can name the entry that said so,
/// and it costs no I/O to be right. See `docs/notes/server-tools-read-image.md`.
fn look(
    cwd: &Path,
    given: &str,
    vision: &crate::llm::Vision,
) -> Result<(ImageRef, String), String> {
    if !vision.accepts_image {
        return Err(format!(
            "`read_image {given}` did not run: {}",
            vision.refusal
        ));
    }
    let path = resolve(cwd, given);
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("`read_image` could not read {}: {e}", path.display()))?;
    let Some(media_type) = media_type(&path, &bytes) else {
        let known = KNOWN.iter().map(|(e, _)| *e).collect::<Vec<_>>().join(", ");
        return Err(format!(
            "`read_image` cannot tell what kind of image {} is: it is not one of the \
             formats this tool recognises ({known}). Name it with one of those \
             extensions, or convert it first.",
            path.display()
        ));
    };
    let image = ImageRef {
        path: path.to_string_lossy().to_string(),
        media_type: media_type.to_string(),
    };
    let text = format!(
        "the model is looking at {} ({}), {} bytes",
        image.path,
        image.media_type,
        bytes.len()
    );
    Ok((image, text))
}

/// `path` as given, resolved against the session's directory when relative.
fn resolve(cwd: &Path, given: &str) -> PathBuf {
    let path = PathBuf::from(given);
    if path.is_absolute() || cwd.as_os_str().is_empty() {
        return path;
    }
    cwd.join(path)
}

/// The media type of `bytes`, from the extension first and the magic number second.
///
/// # Invariant
///
/// A picture is only offered when its kind is certain: a wrong media type is a
/// request the provider rejects, and the model cannot tell why. See
/// `docs/notes/server-tools-read-image.md`.
fn media_type(path: &Path, bytes: &[u8]) -> Option<&'static str> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_follows_the_session_directory() {
        let p = resolve(Path::new("/w/session"), "shot.png");
        assert_eq!(p, Path::new("/w/session/shot.png"));
        let abs = resolve(Path::new("/w/session"), "/tmp/shot.png");
        assert_eq!(abs, Path::new("/tmp/shot.png"));
        // A session without a directory keeps the path as given.
        assert_eq!(resolve(Path::new(""), "shot.png"), Path::new("shot.png"));
    }

    fn vision_of(accepts: bool) -> crate::llm::Vision {
        crate::llm::Vision {
            accepts_image: accepts,
            refusal: crate::config::image_refusal("roster/model-with-text-only", &["text".into()]),
        }
    }

    #[test]
    fn a_model_with_no_image_modality_is_refused_by_name() {
        let d = tempfile::tempdir().unwrap();
        let png = d.path().join("shot.png");
        std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
        let err = look(d.path(), "shot.png", &vision_of(false)).unwrap_err();
        assert!(err.contains("cannot look at images"), "{err}");
        assert!(err.contains("roster/model-with-text-only"), "{err}");
        assert!(err.contains("\"text\""), "{err}");
        assert!(err.contains("shot.png"), "the call itself is named: {err}");
    }

    #[test]
    fn a_picture_is_handed_over_by_path_with_its_media_type() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shot.png"), [0x89, b'P', b'N', b'G']).unwrap();
        let (image, text) = look(d.path(), "shot.png", &vision_of(true)).unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.path, d.path().join("shot.png").to_string_lossy());
        assert!(text.contains("4 bytes"), "{text}");
    }

    #[test]
    fn a_missing_file_says_which_path() {
        let d = tempfile::tempdir().unwrap();
        let err = look(d.path(), "gone.png", &vision_of(true)).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
        assert!(err.contains("gone.png"), "{err}");
    }

    #[test]
    fn a_text_file_is_not_offered_as_a_picture() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("notes.txt"), "hello").unwrap();
        let err = look(d.path(), "notes.txt", &vision_of(true)).unwrap_err();
        assert!(err.contains("cannot tell what kind of image"), "{err}");
        assert!(err.contains("png"), "it lists what it does take: {err}");
    }

    #[test]
    fn the_extension_names_the_type_and_the_bytes_are_the_fallback() {
        assert_eq!(
            media_type(Path::new("a.PNG"), b""),
            Some("image/png"),
            "an extension is matched without regard to case"
        );
        assert_eq!(media_type(Path::new("a.jpg"), b""), Some("image/jpeg"));
        let png = [0x89, b'P', b'N', b'G', 0x0d];
        assert_eq!(media_type(Path::new("noext"), &png), Some("image/png"));
        let gif = b"GIF89a";
        assert_eq!(media_type(Path::new("noext"), gif), Some("image/gif"));
        assert_eq!(media_type(Path::new("notes.txt"), b"hello"), None);
    }
}
