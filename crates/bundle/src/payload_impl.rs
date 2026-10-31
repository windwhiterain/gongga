//! The programs this file carries, and how one lands on disk.
//!
//! See `docs/notes/bundle-main.md`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Puts `bytes` in `dir` under `name`, and says where it sits now.
///
/// # Invariant
///
/// A file already holding these bytes is left alone: this file may be started
/// while the program it carries is running, and no platform replaces a running
/// executable. A different copy is written beside it and renamed in, so no
/// launch ever sees half of one.
pub fn place(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(file_name(name));
    let want = fnv(bytes);
    if fnv_of(&dest).ok() == Some(want) {
        return Ok(dest);
    }
    let temp = dir.join(format!(".{}.{}", file_name(name), std::process::id()));
    {
        let mut out = std::fs::File::create(&temp)?;
        out.write_all(bytes)?;
        out.sync_all()?;
    }
    // A fresh 20 MB exe is what a scanner opens, and Windows answers a rename
    // over one with "access denied" for a moment.
    let mut last = std::io::Error::other("no attempt");
    for attempt in 0..5 {
        match std::fs::rename(&temp, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) => last = e,
        }
        // The running copy could not be replaced; if it is this one, that is the
        // answer the caller wanted anyway.
        if fnv_of(&dest).ok() == Some(want) {
            let _ = std::fs::remove_file(&temp);
            return Ok(dest);
        }
        std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
    }
    let _ = std::fs::remove_file(&temp);
    Err(last)
}

/// FNV-1a, 64 bit: the whole of what "is this the same copy" needs here.
pub fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// The same over a file, in chunks, so a 20 MB program does not become a buffer.
pub fn fnv_of(path: &Path) -> std::io::Result<u64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hash);
        }
        for byte in &buffer[..read] {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// Removes every other build's directory under `dir`, best effort.
///
/// # Invariant
///
/// A directory a process is still running from cannot be removed on Windows, and
/// that is the answer worth having: what is in use stays, what is not goes. One
/// file per release would otherwise leave 35 MB behind on every build.
pub fn prune(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == keep {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// One name for one set of payloads, so a build never rewrites another's files.
pub fn stamp(programs: &[(&str, &[u8])]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, bytes) in programs {
        for byte in name.as_bytes().iter().chain(bytes.iter()) {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// The file name a program is started by on this platform.
pub fn file_name(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}
