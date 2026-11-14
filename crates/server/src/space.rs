//! One path space: the machine a session's paths live on, and how programs run there.
//!
//! See `docs/notes/server-space.md`.

use std::path::Path;

use crate::remote::Endpoint;
use crate::remote::protocol::{Event, Stream};

/// What one program printed, and what it exited with, on one path space.
///
/// # Invariant
///
/// The exit status is the program's own: `None` is a peer that ended without saying, or
/// a connection that died first -- never a guess of `0`, which a caller would read as
/// "it worked".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Run {
    pub exit: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Run {
    /// Whether it ran to its end and said it succeeded.
    pub fn succeeded(&self) -> bool {
        self.exit == Some(0)
    }

    /// Its standard output as text, for a program that prints text.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Where a session's paths live: this machine, or the peer they are on.
///
/// # Invariant
///
/// One value decides both, so no caller asks which machine it is on: a path handed to
/// [`Space::run`] is resolved by the machine that owns it, and a program is started
/// there. See `docs/notes/server-space.md`.
#[derive(Debug, Clone)]
pub struct Space {
    endpoint: Endpoint,
}

impl Space {
    /// This machine, named no way at all.
    pub fn local() -> Self {
        Self {
            endpoint: Endpoint::local(std::path::PathBuf::new()),
        }
    }

    /// The space of one endpoint: its own machine when it is this one, else the peer.
    pub fn of(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }

    /// Whether the paths here are this machine's.
    pub fn is_local(&self) -> bool {
        !self.endpoint.is_remote()
    }

    /// What a caller is told it ran on.
    pub fn describe(&self) -> String {
        self.endpoint.describe()
    }
}

impl Space {
    /// Runs one program to its end, standing in `cwd`, and answers what it printed.
    ///
    /// # Invariant
    ///
    /// `cwd` and `program` are spelled for the machine that owns them: a path this
    /// machine can open is not evidence about a peer's, and a program name is resolved
    /// by that machine's own `PATH`. No clock here: a caller with a deadline wraps this
    /// call in one, because only it knows what its command is worth waiting for.
    pub async fn run(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        stdin: Option<Vec<u8>>,
    ) -> Result<Run, String> {
        match self.is_local() {
            true => run_here(cwd, program, args, stdin).await,
            false => run_there(&self.endpoint, cwd, program, args, stdin).await,
        }
    }
}

/// One program on this machine: no protocol, because there is nobody to speak to.
async fn run_here(
    cwd: &Path,
    program: &str,
    args: &[String],
    stdin: Option<Vec<u8>>,
) -> Result<Run, String> {
    use tokio::io::AsyncWriteExt;
    let mut command = tokio::process::Command::new(program);
    if !cwd.as_os_str().is_empty() {
        command.current_dir(cwd);
    }
    command
        .args(args)
        .stdin(match stdin.is_some() {
            true => std::process::Stdio::piped(),
            false => std::process::Stdio::null(),
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    // Dropped here, so a program reading its standard input sees the end of it.
    if let Some(bytes) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        pipe.write_all(&bytes)
            .await
            .map_err(|e| format!("could not write to {program}: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("waiting for {program}: {e}"))?;
    Ok(Run {
        exit: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// One program on the peer, through its runner: the same three facts, over the wire.
async fn run_there(
    endpoint: &Endpoint,
    cwd: &Path,
    program: &str,
    args: &[String],
    stdin: Option<Vec<u8>>,
) -> Result<Run, String> {
    let runner = crate::remote::pick::runner(endpoint, endpoint.answer_timeout())
        .await
        .map_err(|e| e.to_string())?;
    // The directory is passed, never resolved here: it is a path as that machine spells it.
    let at = cwd.to_string_lossy().into_owned();
    let asked = (!at.trim().is_empty()).then_some(at.as_str());
    let mut shell = runner
        .open_with(asked, Some(program), Some(args))
        .await
        .map_err(|e| e.to_string())?;
    if let Some(bytes) = stdin {
        shell.write(&bytes).await.map_err(|e| e.to_string())?;
    }
    let mut run = Run::default();
    while let Some(event) = shell.next_event().await {
        match event {
            Event::Out { stream, data, .. } => {
                let Some(bytes) = crate::base64::decode(&data) else {
                    continue;
                };
                match stream {
                    Stream::Stdout => run.stdout.extend_from_slice(&bytes),
                    Stream::Stderr => run.stderr.extend_from_slice(&bytes),
                }
            }
            // Its pipes ended: this is the status the peer could prove.
            Event::End { code, .. } => {
                run.exit = code;
                break;
            }
            Event::Hello { .. } => continue,
        }
    }
    // The shell is this connection's, so it is given back before the connection goes.
    let _ = shell.close().await;
    Ok(run)
}
