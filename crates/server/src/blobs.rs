//! The payloads a provider generated, kept beside the home instead of in the row.
//!
//! See `docs/notes/server-blobs.md`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where this process keeps them; unset until a server names its home.
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Creates the directory here, so a first payload need not think about it.
pub fn install(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(
            dir = %dir.display(),
            error = %e,
            "cannot create the blob directory; a provider image will not be replayable"
        );
        return;
    }
    let _ = DIR.set(dir.to_path_buf());
}

/// The directory in force, if a home has been named.
pub fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

/// The file an item's payload lives in, named by the id the provider gave it.
///
/// # Invariant
///
/// Everything that could name a directory is written as its `%XX` escape, so an id
/// cannot point outside this directory, and two ids cannot land on one file -- a
/// lossy map would allow that, and it is a picture silently replaced.
fn file_name(id: &str) -> String {
    let mut safe = String::with_capacity(id.len() + 5);
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            safe.push(c);
        } else {
            for byte in c.encode_utf8(&mut [0u8; 4]).as_bytes() {
                safe.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    if safe.is_empty() {
        safe.push_str("item");
    }
    format!("{safe}.json")
}

/// Stores one payload, as its JSON text so the value's type survives.
pub fn put(dir: &Path, id: &str, text: &str) -> anyhow::Result<()> {
    let name = file_name(id);
    let path = dir.join(&name);
    // Written whole, then renamed; the name is appended, not `with_extension`.
    let tmp = dir.join(format!("{name}.part"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The payload an item id names, or nothing when the file is not there.
pub fn read(dir: &Path, id: &str) -> Option<String> {
    let bytes = std::fs::read(dir.join(file_name(id))).ok()?;
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Removes a payload nothing references; false when it was not there.
pub fn drop(dir: &Path, id: &str) -> bool {
    match std::fs::remove_file(dir.join(file_name(id))) {
        Ok(()) => true,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(id = %id, error = %e, "cannot remove a payload nobody references");
            }
            false
        }
    }
}

/// Removes every orphan named by the database, and says how many files went.
pub fn drop_all(ids: &[String]) -> usize {
    let Some(dir) = dir() else {
        return 0;
    };
    let removed = ids.iter().filter(|id| drop(dir, id)).count();
    if removed > 0 {
        tracing::info!(files = removed, dir = %dir.display(), "provider payloads removed with the session");
    }
    removed
}
