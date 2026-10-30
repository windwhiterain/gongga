//! The tail of this executable: which programs it carries, and how they land.
//!
//! See `docs/notes/bundle-main.md`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The last 16 bytes of a file this crate wrote. Anything else is not a bundle.
///
/// # Invariant
///
/// The tail is `[footer_len: u64][MAGIC]`, so reading the last 24 bytes is enough
/// to find the table and a plain executable cannot be mistaken for one.
pub const MAGIC: &[u8; 16] = b"GONGGA-BUNDLE\x00\x00\x00";

/// One program the file carries, and where its bytes are.
#[derive(Debug, Clone)]
pub struct Program {
    pub name: String,
    pub offset: u64,
    pub len: u64,
    /// A checksum, not a signature: it answers "is this the copy I extracted".
    pub fnv: u64,
}

/// Where the table of a file starts, or `None` when it carries none.
pub fn footer_of(file: &mut File) -> std::io::Result<Option<u64>> {
    let end = file.seek(SeekFrom::End(0))?;
    if end < 24 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(end - 24))?;
    let mut tail = [0u8; 24];
    file.read_exact(&mut tail)?;
    if &tail[8..24] != MAGIC {
        return Ok(None);
    }
    let footer = u64::from_le_bytes(tail[..8].try_into().unwrap_or_default());
    Ok(Some(end - 24 - footer))
}

/// The programs a file carries, read from the table at `start`.
pub fn programs(file: &mut File, start: u64) -> std::io::Result<Vec<Program>> {
    file.seek(SeekFrom::Start(start))?;
    let count = read_u32(file)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut len = [0u8; 2];
        file.read_exact(&mut len)?;
        let mut name = vec![0u8; u16::from_le_bytes(len) as usize];
        file.read_exact(&mut name)?;
        out.push(Program {
            name: String::from_utf8_lossy(&name).into_owned(),
            offset: read_u64(file)?,
            len: read_u64(file)?,
            fnv: read_u64(file)?,
        });
    }
    Ok(out)
}

pub fn fnv_of(path: &Path) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
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

/// Puts `program` in `dir` under the name it is run by, from the bundle at `from`.
///
/// # Invariant
///
/// A file already holding these bytes is left alone: the bundle may be started
/// while the program it carries is running, and Windows refuses to replace a
/// running executable. A different copy is written beside it and renamed in, so
/// no launch ever sees half an executable.
pub fn extract(from: &mut File, program: &Program, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(file_name(&program.name));
    if fnv_of(&dest).ok() == Some(program.fnv) {
        return Ok(dest);
    }
    let temp = dir.join(format!(
        ".{}.{}",
        file_name(&program.name),
        std::process::id()
    ));
    {
        let mut out = File::create(&temp)?;
        from.seek(SeekFrom::Start(program.offset))?;
        let mut left = program.len;
        let mut buffer = vec![0u8; 64 * 1024];
        while left > 0 {
            let want = (left as usize).min(buffer.len());
            from.read_exact(&mut buffer[..want])?;
            out.write_all(&buffer[..want])?;
            left -= want as u64;
        }
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
        if fnv_of(&dest).ok() == Some(program.fnv) {
            let _ = std::fs::remove_file(&temp);
            return Ok(dest);
        }
        std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
    }
    let _ = std::fs::remove_file(&temp);
    Err(last)
}

/// The file name a program is started by on this platform.
pub fn file_name(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn read_u32(file: &mut File) -> std::io::Result<u32> {
    let mut buffer = [0u8; 4];
    file.read_exact(&mut buffer)?;
    Ok(u32::from_le_bytes(buffer))
}

fn read_u64(file: &mut File) -> std::io::Result<u64> {
    let mut buffer = [0u8; 8];
    file.read_exact(&mut buffer)?;
    Ok(u64::from_le_bytes(buffer))
}
