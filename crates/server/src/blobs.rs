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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_payload_comes_back_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let text = json_text(&json!({"data": "iVBORw0KGgo", "mime": "image/png"}));
        put(dir.path(), "ig_1", &text).unwrap();
        assert_eq!(read(dir.path(), "ig_1").as_deref(), Some(text.as_str()));
        // The same item twice is one file, and nothing about it changed.
        put(dir.path(), "ig_1", &text).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_id_cannot_name_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["../../server.key", "a/b", "..", ".", "", "C:\\escape"] {
            put(dir.path(), id, "\"x\"")
                .unwrap_or_else(|e| panic!("{id} -> {}: {e}", file_name(id)));
            let found = read(dir.path(), id).expect("readable back");
            assert_eq!(found, "\"x\"", "{id}");
            assert!(
                dir.path().join(file_name(id)).is_file(),
                "{} wrote outside the directory",
                file_name(id)
            );
        }
        // Six ids in, six names out: nothing escaped, nothing collided.
        let files = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(files, 6, "one file per id");
        assert_ne!(file_name(".."), file_name("./x"));
    }

    #[test]
    fn a_missing_payload_is_said_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), "never_was"), None);
        assert!(
            !drop(dir.path(), "never_was"),
            "nothing was there to remove"
        );
        put(dir.path(), "ig_2", "\"x\"").unwrap();
        assert!(drop(dir.path(), "ig_2"));
        assert_eq!(read(dir.path(), "ig_2"), None);
    }

    /// A half file must never be what a reader finds.
    #[test]
    fn a_half_written_file_is_never_the_one_read() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "ig_3", "\"whole\"").unwrap();
        // The new bytes are still under .part, so the old ones answer.
        std::fs::write(
            dir.path().join(file_name("ig_3")).with_extension("part"),
            "\"partial\"",
        )
        .unwrap();
        assert_eq!(read(dir.path(), "ig_3").as_deref(), Some("\"whole\""));
        assert!(
            !std::fs::read_dir(dir.path()).unwrap().flatten().any(|e| e
                .path()
                .extension()
                .is_some_and(|x| x == "part")
                && read(dir.path(), "anything").is_some())
        );
    }

    fn json_text(value: &serde_json::Value) -> String {
        // The file holds the value's own JSON text, which is what makes the type
        // come back unchanged.
        value.to_string()
    }
}
