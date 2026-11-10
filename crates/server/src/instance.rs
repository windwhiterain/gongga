//! Where this server runs, beside the claim it took. See `docs/notes/server-instance.md`.

use std::path::Path;

/// The directory this server runs in, beside its claim.
///
/// # Invariant
///
/// Inherited from the starter, never chosen here. See `docs/notes/server-instance.md`.
pub(crate) fn record_dir(home: &Path) {
    let path = home.join("server.cwd");
    let written = std::env::current_dir()
        .map(|dir| format!("{}\n", dir.display()))
        .and_then(|text| std::fs::write(&path, text));
    if let Err(e) = written {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "cannot record the directory this server runs in; a handover cannot start its \
             successor from it"
        );
    }
}
