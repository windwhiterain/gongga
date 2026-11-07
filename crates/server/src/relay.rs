//! The relay: one channel's program, and the two files the server talks to it with.
//!
//! See `docs/notes/server-channel.md`.
//!
//! # Invariant
//!
//! This process is what makes a channel outlive the server: it holds the child's
//! stdin, which nobody can re-open, and appends the child's output to a file. It is
//! started detached, so a `cancel` does not end it.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The words the program was started with, read once.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Words {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// What `status` says: enough to know a channel without this process.
///
/// # Invariant
///
/// Rewritten whole by a rename: the server reads it when the relay cannot be asked.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// The program's pid, so `del_job` can end a tree the relay no longer holds.
    pub pid: u32,
    pub started_at: i64,
    /// `None` while it runs.
    pub exit: Option<i32>,
    pub ended_at: Option<i64>,
}

/// How often `in` is looked at. The floor on a write's round trip.
const POLL: Duration = Duration::from_millis(50);

/// Runs one channel until its program ends.
pub fn run(dir: &Path) -> anyhow::Result<()> {
    let words: Words = serde_json::from_slice(&std::fs::read(dir.join("cmd"))?)?;
    let mut command = Command::new(&words.program);
    command
        .args(&words.args)
        .current_dir(&words.cwd)
        .envs(words.env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so stopping this channel can take the whole tree.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting {:?}: {e}", words.program))?;
    let pid = child.id();
    // Its streams are pumped into `out` here: Git's bash cannot write to an inherited
    // handle.
    let log = std::sync::Arc::new(std::sync::Mutex::new(append(&dir.join("out"))?));
    if let Some(pipe) = child.stdout.take() {
        let log = std::sync::Arc::clone(&log);
        std::thread::spawn(move || pump(pipe, log));
    }
    if let Some(pipe) = child.stderr.take() {
        let log = std::sync::Arc::clone(&log);
        std::thread::spawn(move || pump(pipe, log));
    }
    // Armed so `kill` takes what the program started, not only the program.
    #[cfg(windows)]
    let mut chair = {
        let chair = crate::tools::job::ProcessJob::own();
        if let Some(chair) = &chair {
            chair.assign(pid);
        }
        chair
    };
    let started_at = now_millis();
    write_status(
        dir,
        &Status {
            pid,
            started_at,
            exit: None,
            ended_at: None,
        },
    )?;
    // The command file is the server's to create: its first line must not be missed.
    let commands = dir.join("in");
    let mut offset: u64 = 0;
    loop {
        offset = drain(&commands, offset, &mut child)?;
        if let Some(exit) = ended(&mut child)? {
            let status = Status {
                pid,
                started_at,
                exit: Some(exit),
                ended_at: Some(now_millis()),
            };
            write_status(dir, &status)?;
            #[cfg(windows)]
            if let Some(chair) = chair.take() {
                chair.release();
            }
            // What it printed is in `out` already: its own handles wrote it.
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
}

/// The child's status, when it has ended.
fn ended(child: &mut Child) -> anyhow::Result<Option<i32>> {
    match child.try_wait()? {
        Some(status) => Ok(Some(status.code().unwrap_or(0))),
        None => Ok(None),
    }
}

/// Copies one stream into the log for as long as it says anything.
///
/// # Invariant
///
/// Every chunk is flushed as it lands: a reader of `out` is waiting for it, and a
/// channel whose output sat in this process's buffer would look silent.
fn pump<R: Read>(mut pipe: R, log: std::sync::Arc<std::sync::Mutex<std::fs::File>>) {
    let mut buffer = [0u8; 4096];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                let _ = log.write_all(&buffer[..read]);
                let _ = log.flush();
            }
            Err(_) => break,
        }
    }
}

/// Applies every command appended to `in` since `offset`.
///
/// # Invariant
///
/// One line, one command: `input <base64>` writes to stdin, `kill` ends the program.
fn drain(path: &Path, offset: u64, child: &mut Child) -> anyhow::Result<u64> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok(offset);
    };
    let length = file.metadata()?.len();
    if length <= offset {
        return Ok(offset);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut fresh = String::new();
    file.read_to_string(&mut fresh)?;
    // A line still being written is left for the next look.
    let complete = match fresh.rfind('\n') {
        Some(at) => at + 1,
        None => return Ok(offset),
    };
    let (whole, _) = fresh.split_at(complete);
    for line in whole.lines() {
        match line.trim_end() {
            "kill" => {
                if let Some(stdin) = child.stdin.take() {
                    drop(stdin);
                }
                #[cfg(unix)]
                {
                    // The whole group: a program started with `&` is still its child.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                child.kill()?;
            }
            other => {
                let Some(encoded) = other.strip_prefix("input ") else {
                    tracing::warn!(file = %path.display(), "`in` said {other:?}, which is no command");
                    continue;
                };
                let Some(bytes) = crate::base64::decode(encoded.trim()) else {
                    tracing::warn!("`in` carried an input that is not base64; it is dropped");
                    continue;
                };
                if let Some(stdin) = child.stdin.as_mut() {
                    stdin.write_all(&bytes)?;
                    stdin.flush()?;
                }
            }
        }
    }
    Ok(offset + complete as u64)
}

/// Opens a file for appending, creating it, so two writers interleave and neither
/// truncates.
fn append(path: &Path) -> anyhow::Result<std::fs::File> {
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(path)?)
}

/// Writes `status` where a reader will find it whole.
fn write_status(dir: &Path, status: &Status) -> anyhow::Result<()> {
    let text = serde_json::to_vec(status)?;
    let writing = dir.join("status.writing");
    std::fs::write(&writing, &text)?;
    std::fs::rename(&writing, dir.join("status"))?;
    Ok(())
}

/// Milliseconds since the epoch, as the rest of this server spells them.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}
