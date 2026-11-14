//! The files an older build wrote into `<home>/lib`; see docs/notes/server-retired-python.md.

use std::path::Path;

/// Where an older build put them: a fixed `lib/` in the home, not a content mapping.
const LIB: &str = "lib";

/// The bytes this build wrote as `gongga.py`, before the content shipped the name.
const GONGGA: &[u8] = include_bytes!("retired/python/gongga.py");

/// The same, for `gongga_cli.py`.
const GONGGA_CLI: &[u8] = include_bytes!("retired/python/gongga_cli.py");

/// The same, for `gongga_skill.py`.
const GONGGA_SKILL: &[u8] = include_bytes!("retired/python/gongga_skill.py");

/// The three names, and the bytes this build wrote at each of them.
///
/// # Invariant
///
/// Copies, not hashes: a hash is a dependency and still only a witness, while a
/// whole-bytes match is the identity of the file, and a wrong witness here deletes
/// a reader's own module. These are the bytes of a feature this repository no
/// longer has; nothing reads them, and nothing but `retire` may.
const RETIRED: [(&str, &[u8]); 3] = [
    ("gongga.py", GONGGA),
    ("gongga_cli.py", GONGGA_CLI),
    ("gongga_skill.py", GONGGA_SKILL),
];

/// Deletes them where they are still exactly what an older build wrote.
///
/// # Invariant
///
/// Only whole-byte equality removes a file. `<home>/lib` is where the content
/// installs and where the reader may write, so a file there that differs is
/// theirs: it is left alone, and the content's own reconcile is what reports it.
/// This retires a build's own artifacts, never the content's.
pub fn retire(home: &Path) {
    let mut removed: Vec<&str> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    for (name, written) in RETIRED {
        let path = home.join(LIB).join(name);
        let Ok(found) = std::fs::read(&path) else {
            continue;
        };
        if found != written {
            kept.push(name);
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(name),
            Err(error) => tracing::warn!(
                path = %path.display(),
                %error,
                "retired python: the file is this build's, and could not be removed"
            ),
        }
    }
    if removed.is_empty() && kept.is_empty() {
        return;
    }
    tracing::info!(
        removed = ?removed,
        kept = ?kept,
        "retired python: a file goes only where its bytes are the ones an older build wrote, \
         so what is kept here is the reader's"
    );
}
