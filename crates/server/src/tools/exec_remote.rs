//! `exec` on the session's remote: one program, its arguments, no shell.
//!
//! # Invariant
//!
//! The peer starts the program with the words it was given, the same way this
//! machine does: no shell reads them, so nothing between the words and the
//! program can interpret them.

use std::time::Duration;

use crate::remote::endpoint::Endpoint;
use crate::remote::protocol::{Event, Stream};

/// What one run on the peer produced.
pub struct RemoteOutcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Runs the program on the peer and waits for it.
///
/// # Invariant
///
/// The program is started with its words as arguments and no shell between it
/// and the words, so what the program receives is what the caller wrote.
pub async fn run(
    endpoint: &Endpoint,
    program: &str,
    args: &[String],
    cwd: &str,
    timeout: Duration,
) -> Result<RemoteOutcome, String> {
    let asked = (!cwd.trim().is_empty()).then_some(cwd);
    let mut handle = endpoint
        .connect_program(asked, Some(program), Some(args))
        .await
        .map_err(|e| format!("exec could not start {program:?} on the peer: {e}"))?;

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut code: Option<i32> = None;
    let deadline = tokio::time::Instant::now() + timeout;

    // The peer ends the program with an `End`; what it printed comes before it.
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            let _ = handle.control().close().await;
            return Err(format!(
                "{program} did not finish within {}s on the peer and was stopped",
                timeout.as_secs()
            ));
        }
        let next = tokio::time::timeout_at(deadline, handle.next_event()).await;
        let event = match next {
            Err(_) => {
                let _ = handle.control().close().await;
                return Err(format!(
                    "{program} did not finish within {}s on the peer and was stopped",
                    timeout.as_secs()
                ));
            }
            Ok(None) => break,
            Ok(Some(event)) => event,
        };
        match event {
            Event::Out { stream, data, .. } => {
                let Some(bytes) = crate::base64::decode(&data) else {
                    let _ = handle.control().close().await;
                    return Err("the peer sent output this build cannot read".to_string());
                };
                match stream {
                    Stream::Stdout => stdout.extend_from_slice(&bytes),
                    Stream::Stderr => stderr.extend_from_slice(&bytes),
                }
            }
            Event::End { code: ended, .. } => {
                code = ended;
                break;
            }
            Event::Hello { .. } => {}
        }
    }
    let _ = handle.control().close().await;
    Ok(RemoteOutcome {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        code: code.unwrap_or(-1),
    })
}
