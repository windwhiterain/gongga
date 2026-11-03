//! The pictures a message named, copied into this home so a row outlives its file.
//!
//! See `docs/notes/server-pictures.md`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where this process keeps them; unset until a server names its home.
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Creates the directory here, so a first picture need not think about it.
pub fn install(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(
            dir = %dir.display(),
            error = %e,
            "cannot create the picture directory; a message naming one keeps only its path"
        );
        return;
    }
    let _ = DIR.set(dir.to_path_buf());
}

fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

/// The copy a row recorded, when it recorded one; otherwise the bytes at its path.
///
/// # Invariant
///
/// A copy beats a path: a picture the user has since overwritten is still the picture
/// that was sent. The path is read only for a row written before copies were kept.
pub fn read_missing(image: &crate::image::ImageRef) -> Option<Vec<u8>> {
    image
        .copy
        .and_then(read)
        .or_else(|| std::fs::read(&image.path).ok())
}

/// The copy `id` names, or nothing when this home holds none.
pub fn read(id: i64) -> Option<Vec<u8>> {
    let bytes = std::fs::read(dir()?.join(id.to_string())).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

/// Writes one copy under the id a row already records.
fn write(id: i64, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = dir().ok_or_else(|| {
        anyhow::anyhow!("this process has no home installed, so a picture has nowhere to go")
    })?;
    // Whole, then renamed: a half-written copy would be a picture that reads as broken.
    let temporary = dir.join(format!("{id}.part"));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, dir.join(id.to_string()))?;
    Ok(())
}

/// The copy this home already holds for `path` and these bytes, or a new one.
///
/// # Invariant
///
/// One copy per picture, keyed by the path it was named with, and a rewritten file is
/// a new copy: a message that was already sent keeps showing the bytes it carried.
/// A copy with no row, or a row with no copy, degrades to no picture rather than to a
/// wrong one.
pub fn remember(
    store: &crate::store::Store,
    path: &str,
    media_type: &str,
    bytes: &[u8],
) -> anyhow::Result<i64> {
    if let Some((id, kept_media)) = store.picture_of(path)
        && kept_media == media_type
        && read(id).is_some_and(|kept| kept == bytes)
    {
        return Ok(id);
    }
    let id = store.add_picture(path, media_type)?;
    if let Err(e) = write(id, bytes) {
        // A row naming a copy that was never written is a hole; the mention keeps
        // the path it was given instead.
        store.drop_picture(id);
        return Err(e);
    }
    Ok(id)
}
