//! The harness's Python surface, installed beside the home.
//!
//! See `docs/notes/server-pylib.md`.

use std::path::{Path, PathBuf};

/// The contract a skill package is run through; the skill crate carries it too.
const GONGGA_SKILL: &str = include_str!("../../../python/gongga_skill.py");

/// A client for this server: one function per command it publishes.
const GONGGA: &str = include_str!("../../../python/gongga.py");

/// Writes both modules under `<home>/lib`, and answers that directory.
///
/// # Invariant
///
/// One file, several readers: the skill crate's `-c` driver and this installer carry
/// the same bytes, and a session's Python imports them from here. A file is written
/// only when its bytes differ, so a restart never rewrites what a caller is reading.
pub fn install(home: &Path) -> std::io::Result<PathBuf> {
    let dir = home.join("lib");
    std::fs::create_dir_all(&dir)?;
    for (name, source) in [("gongga_skill.py", GONGGA_SKILL), ("gongga.py", GONGGA)] {
        let file = dir.join(name);
        if std::fs::read_to_string(&file).ok().as_deref() != Some(source) {
            std::fs::write(&file, source)?;
        }
    }
    Ok(dir)
}
