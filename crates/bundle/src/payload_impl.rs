//! The programs this file carries, and how one lands on disk.
//!
//! See `docs/notes/bundle-main.md`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Never shown: a console of the program's own, with no window on the desktop.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
    // The payload is a program: `take_over` spawns it directly, so it needs the
    // execute bit. Windows has none to set.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
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

/// The file that says which build placed a directory.
///
/// # Invariant
///
/// Only a directory holding this marker is this file's to stop or to remove: a home's
/// `payload/` also holds a peer's pair for one protocol (`remote/pick.rs`), which this
/// file did not put there and may not touch.
pub const MARKER: &str = ".payload";

/// Says this directory holds `stamp`'s programs.
pub fn mark(dir: &Path, stamp: &str) -> std::io::Result<()> {
    std::fs::write(dir.join(MARKER), stamp)
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

/// Stops the server another build left running, then removes its directory.
///
/// # Invariant
///
/// Asked, not killed: a server that receives `cancel` keeps its turn as pending
/// work (`commands.rs`). A window or a command in flight is never touched, so a
/// build whose clients are still up keeps its directory until they exit. `ours`
/// is the payload of the build asking. `keeps_servers` leaves the port alone;
/// `force` rides into the ask, so a server that refuses it is stopped by pid
/// instead of left holding the port. See `bundle-main.md`.
pub fn retire(dir: &Path, keep: &str, ours: &Path, keeps_servers: bool, force: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == keep {
            continue;
        }
        let old = entry.path();
        // Before anything is asked to stop: a directory this feature did not place is
        // not a build of ours to replace.
        if !old.join(MARKER).is_file() {
            continue;
        }
        if !stop_programs_in(&old, ours, keeps_servers, force) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&old);
    }
}

/// Stops the server running from `dir`, and says whether the directory can go.
///
/// # Invariant
///
/// `keeps_servers` says the program about to run starts no server of its own:
/// the one that is running keeps the port, and the directory stays with it.
/// `force` stops only the pid `running_in(dir)` named, never a process found by
/// its image name. See `bundle-main.md`.
fn stop_programs_in(dir: &Path, ours: &Path, keeps_servers: bool, force: bool) -> bool {
    let running = running_in(dir);
    let Some(server) = running.iter().find(|process| !outlives_its_build(process)) else {
        // Only clients: nothing holds a port, and their directory is theirs.
        return running.is_empty();
    };
    if keeps_servers {
        // Nothing is waiting for the port, and the server keeps answering the
        // window that was told to use it.
        return false;
    }
    // `force` travels with the ask instead of replacing it: the command line knows
    // how to stop a build its own plain `cancel` cannot reach, and only a stop that
    // was refused is a stop to kill.
    if !ask_to_cancel(ours, dir, server, force) {
        kill(server.pid);
        wait_until_gone(server.pid);
    }
    // The port is free either way; the directory waits for a client still running.
    !alive(server.pid) && running.len() == 1
}

/// Whether `process` may outlive the build that started it.
///
/// # Invariant
///
/// A window is where the reader is typing and a `cli` call is a command halfway
/// through; killing either to free a directory would be a build upgrade taking
/// work with it. Only a server is replaced, because only a server holds the port.
fn outlives_its_build(process: &Process) -> bool {
    process.name != "gongga-server"
}

/// Whether a server running from `dir` answered a `cancel` and stopped.
///
/// # Invariant
///
/// `force` is passed on, not consumed here: the command line is this home's, it may
/// be a build other than this one, and what it can do about a server that will not
/// stop is its own business. This file only falls back to a kill when the answer is
/// no.
fn ask_to_cancel(ours: &Path, dir: &Path, server: &Process, force: bool) -> bool {
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
    // The home of the build asking, not the one being retired: that is where the
    // command line lives.
    let Some(home) = ours.parent().and_then(|payload| payload.parent()) else {
        return false;
    };
    let Some(mut cancel) = command_line(home) else {
        return false;
    };
    let Some(port) = server.port else {
        return false;
    };
    if force {
        cancel.arg("--force");
    }
    cancel
        .arg("--server")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--key")
        .arg(key)
        .arg("cancel")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // The bundle has no console to hand down: the command would get a window.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cancel.creation_flags(CREATE_NO_WINDOW);
    }
    let cancel = cancel.status();
    if !cancel.is_ok_and(|status| status.success()) {
        return false;
    }
    // `cancel` answers before the server stops; give it the moment it took.
    wait_until_gone(server.pid)
}

/// The command line this home answers with, ready for `cancel` and its arguments.
///
/// # Invariant
///
/// Asked by the name a shell types, never by a copy in this payload: the command
/// layer is this home's Python, which a server writes into `bin` on every start
/// (`docs/notes/bundle-main.md`). Carrying one would ask with a build the operator
/// may have replaced, and a head that knows where the layer lives would have to
/// learn it again the next time the layer moves.
fn command_line(home: &Path) -> Option<std::process::Command> {
    let bin = home.join("bin");
    // A home whose server predates the move left a program here; it answers too.
    let native = bin.join(file_name("gongga-cli"));
    if native.is_file() {
        return Some(std::process::Command::new(native));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `CreateProcess` will not run a `.cmd`; `cmd` will, given one line.
        let shim = bin.join("gongga-cli.cmd");
        if shim.is_file() {
            let mut command = std::process::Command::new("cmd");
            command.arg("/C").raw_arg(format!("\"{}\"", shim.display()));
            return Some(command);
        }
    }
    None
}

/// Whether `pid` went away within the moment a stop is allowed to take.
fn wait_until_gone(pid: u32) -> bool {
    for _ in 0..60 {
        if !alive(pid) {
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
            if !matches!(
                name.as_str(),
                "gongga-server" | "gongga-tui" | "gongga-client" | "gongga-cli"
            ) {
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
