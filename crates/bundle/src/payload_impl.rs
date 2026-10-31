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

/// Stops the programs another build left running, then removes its directory.
///
/// # Invariant
///
/// Asked, not killed: a server that receives `cancel` keeps its turn as pending
/// work (`commands.rs`). Files go only once nothing of that build runs.
/// `ours` is the command layer that asks, because the retired one may be gone.
pub fn retire(dir: &Path, keep: &str, ours: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == keep {
            continue;
        }
        let old = entry.path();
        if !stop_programs_in(&old, ours) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&old);
    }
}

/// Stops every process running from `dir`, and says whether none is left.
fn stop_programs_in(dir: &Path, ours: &Path) -> bool {
    let running = running_in(dir);
    let server = running
        .iter()
        .find(|process| process.name.starts_with("gongga-server"))
        .cloned();
    let mut stopped = true;
    if let Some(process) = server.clone() {
        stopped = ask_to_cancel(ours, dir, &process);
        if !stopped {
            // No key, no command layer, or no answer: a stale server holding
            // the port is the worse outcome.
            kill(process.pid);
            stopped = !alive(process.pid);
        }
    }
    for process in &running {
        if Some(process.pid) == server.as_ref().map(|server| server.pid) {
            continue;
        }
        kill(process.pid);
        stopped &= !alive(process.pid);
    }
    stopped
}

/// Whether a server running from `dir` answered a `cancel` and stopped.
fn ask_to_cancel(ours: &Path, dir: &Path, server: &Process) -> bool {
    // The key is the home's, two levels up from a build in `payload/<stamp>`.
    let home = dir.parent().and_then(|payload| payload.parent());
    let key = home
        .map(|home| home.join("server.key"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty());
    let Some(key) = key else {
        return false;
    };
    let program = ours.join(file_name("gongga-cli"));
    if !program.is_file() {
        return false;
    }
    let Some(port) = server.port else {
        return false;
    };
    let cancel = std::process::Command::new(&program)
        .arg("--server")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--key")
        .arg(key)
        .arg("cancel")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !cancel.is_ok_and(|status| status.success()) {
        return false;
    }
    // `cancel` answers before the server stops; give it the moment it took.
    for _ in 0..60 {
        if !alive(server.pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// One process of a retired build: which program, its pid, and the port it was
/// told to bind.
#[derive(Debug, Clone)]
struct Process {
    name: String,
    pid: u32,
    port: Option<u16>,
}

/// The programs of `dir` that are running right now.
fn running_in(dir: &Path) -> Vec<Process> {
    processes()
        .into_iter()
        .filter_map(|(pid, image, args)| {
            let image = image?;
            if image.parent() != Some(dir) {
                return None;
            }
            let name = image
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !matches!(name.as_str(), "gongga-server" | "gongga-tui" | "gongga-cli") {
                return None;
            }
            Some(Process {
                name,
                pid,
                port: port_in(&args),
            })
        })
        .collect()
}

/// The port a process was told to bind: `--bind 127.0.0.1:<p>` or `--port <p>`.
fn port_in(args: &str) -> Option<u16> {
    let words: Vec<&str> = args.split_whitespace().collect();
    for (at, word) in words.iter().enumerate() {
        if let Some(bind) = word.strip_prefix("--bind=") {
            return bind.rsplit(':').next()?.parse().ok();
        }
        if let Some(port) = word.strip_prefix("--port=") {
            return port.parse().ok();
        }
        if *word == "--bind" || *word == "--port" {
            let value = words.get(at + 1)?;
            return value.rsplit(':').next()?.parse().ok();
        }
    }
    None
}

/// Whether `pid` is a live process.
#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    // `tasklist` is on every Windows, and asking it is cheaper than a Win32 call
    // this crate has no dependency for.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("tasklist")
        .args(["/NH", "/FI", &format!("PID eq {pid}")])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
        .unwrap_or(true)
}

#[cfg(not(windows))]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

/// Ends `pid` and what it started.
fn kill(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

/// Every process on this machine: pid, the executable, and its command line.
#[cfg(windows)]
fn processes() -> Vec<(u32, Option<PathBuf>, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // The full path, because two builds of one name differ by directory.
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId)|$($_.ExecutablePath)|$($_.CommandLine)\" }",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '|');
            let pid: u32 = fields.next()?.trim().parse().ok()?;
            let path = fields.next().unwrap_or_default().trim().to_string();
            let args = fields.next().unwrap_or_default().trim().to_string();
            Some((pid, (!path.is_empty()).then(|| PathBuf::from(path)), args))
        })
        .collect()
}

#[cfg(not(windows))]
fn processes() -> Vec<(u32, Option<PathBuf>, String)> {
    let out = std::process::Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pid, args) = line.split_once(char::is_whitespace)?;
            let args = args.trim().to_string();
            let program = args.split_whitespace().next().unwrap_or_default();
            Some((
                pid.parse().ok()?,
                (!program.is_empty()).then(|| PathBuf::from(program)),
                args.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build this one replaces is removed, and the current one is not.
    #[test]
    fn retire_removes_a_directory_nothing_runs_from() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(file_name("gongga-cli")), b"old").unwrap();
        let keep = dir.path().join("keep");
        std::fs::create_dir_all(&keep).unwrap();

        retire(dir.path(), "keep", &keep);

        assert!(!old.exists(), "the replaced build is gone");
        assert!(keep.exists(), "the current one is not");
    }

    /// A stamp is one per set of bytes: two builds that differ get two names, and
    /// the same build always the same one.
    #[test]
    fn a_stamp_follows_the_bytes() {
        let one = stamp(&[("gongga-cli", b"a"), ("gongga-server", b"b")]);
        let same = stamp(&[("gongga-cli", b"a"), ("gongga-server", b"b")]);
        let other = stamp(&[("gongga-cli", b"a"), ("gongga-server", b"c")]);
        assert_eq!(one, same);
        assert_ne!(one, other);
    }
}
