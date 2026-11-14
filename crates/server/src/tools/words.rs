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
/// The program is spawned with its words as arguments and no shell between them, so
/// what it receives is what the caller wrote. `stdin` is the call itself when a
/// program reads one, written while it runs so a long request cannot deadlock.
/// Both streams are read while it runs; a run past the deadline is stopped.
/// The directory is `standing_dir`'s: a missing one is this server's own.
pub async fn run(
    program: &Path,
    args: &[String],
    stdin: Option<&str>,
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<RunOutcome, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(crate::tools::standing_dir(cwd))
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(match stdin {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;

    // The request, on its own task: a slow child must not block the write, and the
    // dropped pipe is its EOF.
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = pipe.write_all(text.as_bytes()).await;
        });
    }

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
