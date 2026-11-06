//! One program, its words, and what it printed: the spawn both tools share.
//!
//! See `docs/notes/server-tools-words.md`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// What one run produced.
pub struct RunOutcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Runs the program and waits for it, however long it takes.
///
/// # Invariant
///
/// The program is spawned with its words as arguments and no shell between them,
/// so what it receives is what the caller wrote. Both streams are read while it
/// runs, so a chatty program cannot fill a pipe and stop; a run that passes the
/// deadline is stopped, not left behind.
pub async fn run(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<RunOutcome, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| spawn_error(program, &e))?;

    let out_task = child
        .stdout
        .take()
        .map(|stream| tokio::spawn(drain(stream)));
    let err_task = child
        .stderr
        .take()
        .map(|stream| tokio::spawn(drain(stream)));

    let waited = tokio::time::timeout(timeout, child.wait()).await;
    let code = match waited {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return Err(format!("waiting for {}: {e}", program.display())),
        // The deadline passed: the program is stopped, not left running.
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let (stdout, stderr) = drain_both(out_task, err_task).await;
            return Err(format!(
                "{} did not finish within {}s and was stopped; it printed:\n{}{}",
                program.display(),
                timeout.as_secs(),
                stdout,
                stderr
            ));
        }
    };
    let (stdout, stderr) = drain_both(out_task, err_task).await;
    Ok(RunOutcome {
        stdout,
        stderr,
        code,
    })
}

/// One program of its own, printing into a file.
///
/// # Invariant
///
/// Started outside this server's job object, with `kill_on_drop` off: the wait can end
/// without the work ending, which is what a call that outlives its clock is. See
/// `docs/notes/server-tools-exec.md`.
pub struct Own {
    pub pid: u32,
    /// Both streams, in the order they were written: one answer, not two.
    pub log: std::path::PathBuf,
    pub child: tokio::process::Child,
}

/// Starts one program the way a job is started: a group of its own, a log of its own.
pub fn spawn_own(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    log: &Path,
) -> Result<Own, String> {
    let mut launched =
        crate::tools::detached::DetachedJob::launch_program(program, args, cwd, log, env)
            .map_err(|e| format!("could not run {}: {e}", program.display()))?;
    let pid = launched.pid;
    let child = launched
        .take_child()
        .ok_or_else(|| format!("{} started without a handle to wait on", program.display()))?;
    Ok(Own {
        pid,
        log: log.to_path_buf(),
        child,
    })
}

/// Reads one stream to its end.
async fn drain(mut stream: impl tokio::io::AsyncRead + Unpin) -> String {
    use tokio::io::AsyncReadExt;
    let mut text = Vec::new();
    let _ = stream.read_to_end(&mut text).await;
    String::from_utf8_lossy(&text).into_owned()
}

/// What both streams said, each read to its end.
async fn drain_both(
    out: Option<tokio::task::JoinHandle<String>>,
    err: Option<tokio::task::JoinHandle<String>>,
) -> (String, String) {
    let stdout = match out {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    let stderr = match err {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    (stdout, stderr)
}

/// Why the program did not start, in words a reader acts on.
fn spawn_error(program: &Path, error: &std::io::Error) -> String {
    let program = program.display();
    match error.kind() {
        std::io::ErrorKind::NotFound => format!(
            "no program named {program:?} could be started: a name is looked up on PATH, \
             and a word with a directory in it is run as it is written"
        ),
        std::io::ErrorKind::PermissionDenied => {
            format!("running {program:?} was not allowed")
        }
        _ => format!("could not run {program:?}: {error}"),
    }
}
