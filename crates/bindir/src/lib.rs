//! The directory the harness's own programs live in: how one lands there, and whether
//! the copy answering to a name is this build's.
//!
//! SPEC, "Bundled binary": programs placed in one directory "auto detect, validate and
//! install each other with latest compatible protocol". This module is that sentence,
//! written once. See `docs/notes/bindir.md`.
//!
//! # Invariant
//!
//! One implementation for every pair: the server's `bin`, the bundle's payload, a
//! peer's directory, the command layer's own copy. Comparing bytes, writing beside,
//! renaming in, retrying a name a scanner holds, and what a refusal means are one act,
//! so a case any caller needs is a case every caller gets.
//!
//! # Invariant
//!
//! A copy that is not these bytes is never reported as placed: [`Outcome::Pinned`] names
//! what answers to the name instead, and [`Outcome`] is `#[must_use]`, so no caller can
//! read a stale program as this build's.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// How many times a rename over a fresh program is tried before the name is called held.
///
/// A freshly written executable is what a scanner opens, and Windows answers a rename
/// over one with "access denied" for a moment.
const RENAME_ATTEMPTS: u32 = 5;

/// Where the bytes come from: a program beside this one, or a payload a file carries.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Bytes(&'a [u8]),
    File(&'a Path),
}

/// What the directory holds after an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It already held these bytes.
    Already(PathBuf),
    /// Written beside and renamed in: the directory holds these bytes now.
    Placed(PathBuf),
    /// A running program holds the name, so this start cannot replace it. `installed` is
    /// what answers to it, and it is not this build's bytes.
    Pinned { installed: PathBuf, why: String },
}

impl Outcome {
    /// The path that answers to the name, whatever bytes it holds.
    pub fn path(&self) -> &Path {
        match self {
            Outcome::Already(path) | Outcome::Placed(path) => path,
            Outcome::Pinned { installed, .. } => installed,
        }
    }

    /// Whether the directory now holds these bytes.
    pub fn is_this_build(&self) -> bool {
        !matches!(self, Outcome::Pinned { .. })
    }
}

/// Puts `source` in `dir` under `name`, and says what the directory holds.
///
/// # Invariant
///
/// Written beside and renamed in, never in place: no launcher sees half a program. A
/// copy that is running now cannot be replaced on Windows, so that answer is [`Outcome::
/// Pinned`] rather than an error -- the caller decides, and cannot pretend it was placed.
pub fn install(dir: &Path, name: &str, source: Source<'_>) -> std::io::Result<Outcome> {
    std::fs::create_dir_all(dir)?;
    let installed = dir.join(file_name(name));
    let want = identity(source).ok();
    if want.is_some() && identify(&installed) == want {
        return Ok(Outcome::Already(installed));
    }
    let temp = dir.join(format!(".{}.{}", file_name(name), std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let written = match &source {
        Source::Bytes(bytes) => write_bytes(&temp, bytes),
        Source::File(from) => write_file(from, &temp),
    };
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    let mut last = None;
    for attempt in 0..RENAME_ATTEMPTS {
        match std::fs::rename(&temp, &installed) {
            Ok(()) => {
                // One directory holds one answer to the lookup `PATH` does.
                if let Some(other) = other_spelling(name) {
                    let _ = std::fs::remove_file(dir.join(other));
                }
                return Ok(Outcome::Placed(installed));
            }
            Err(e) => last = Some(e),
        }
        if want.is_some() && identify(&installed) == want {
            let _ = std::fs::remove_file(&temp);
            return Ok(Outcome::Already(installed));
        }
        std::thread::sleep(std::time::Duration::from_millis(
            50 * u64::from(attempt + 1),
        ));
    }
    let _ = std::fs::remove_file(&temp);
    match (last, installed.is_file()) {
        (Some(e), true) => Ok(Outcome::Pinned {
            installed,
            why: e.to_string(),
        }),
        (Some(e), false) => Err(e),
        (None, _) => Err(std::io::Error::other("no attempt was made")),
    }
}

/// This build's bytes under a name no other build asks for, for a caller whose own name is
/// held by a running program.
///
/// # Invariant
///
/// The answer holds these bytes or there is none: a caller that cannot have them here is
/// told so, rather than handed the copy that is here instead.
pub fn install_aside(dir: &Path, name: &str, source: Source<'_>) -> std::io::Result<PathBuf> {
    let bytes = match &source {
        Source::Bytes(bytes) => bytes.to_vec(),
        Source::File(from) => std::fs::read(from)?,
    };
    let tagged = format!(
        "{}-{:016x}",
        name,
        identity(Source::Bytes(&bytes)).unwrap_or(0)
    );
    match install(dir, &tagged, Source::Bytes(&bytes))? {
        Outcome::Already(path) | Outcome::Placed(path) => Ok(path),
        Outcome::Pinned { why, .. } => Err(std::io::Error::other(why)),
    }
}

/// Whether `dir` already holds exactly these bytes under `name`.
pub fn holds(dir: &Path, name: &str, source: Source<'_>) -> bool {
    match (identity(source).ok(), identify(&dir.join(file_name(name)))) {
        (Some(want), Some(there)) => want == there,
        _ => false,
    }
}

/// The file name a program is started by on this platform.
pub fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The spelling this platform does not use, when the other one is possible at all.
fn other_spelling(name: &str) -> Option<String> {
    (file_name(name) != name).then(|| name.to_string())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut out = std::fs::File::create(path)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    executable(path)
}

/// A link where one can be made, a copy where it cannot cross a volume.
fn write_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::hard_link(from, to).is_ok() {
        return executable(to);
    }
    std::fs::copy(from, to).map(|_| ())
}

/// The payload is a program: it is started directly, so it needs the execute bit.
/// Windows has none to set.
fn executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// The bytes a source names, hashed.
fn identity(source: Source<'_>) -> std::io::Result<u64> {
    match source {
        Source::Bytes(bytes) => Ok(mix(bytes, 0xcbf2_9ce4_8422_2325)),
        Source::File(from) => identify(from).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no readable program at {}", from.display()),
            )
        }),
    }
}

/// The same hash for a file, streamed: a 20 MB payload is not read into memory to be
/// compared.
fn identify(path: &Path) -> Option<u64> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            return Some(hash);
        }
        hash = mix(&buffer[..read], hash);
    }
}

/// FNV-1a, 64 bit: the hash `identity` is built from, and nothing more.
fn mix(bytes: &[u8], mut hash: u64) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
