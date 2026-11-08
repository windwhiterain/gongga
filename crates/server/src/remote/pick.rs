//! Which runner a peer runs: one that speaks this build's protocol.
//!
//! See `docs/notes/server-remote.md`.
//!
//! # Invariant
//!
//! The answer is this build's own runner, placed on the peer under the name of this
//! build. A configuration may name a program, and it is preferred, but only what the
//! peer says at its greeting decides: a mismatch is not argued with, it is run past.
//! That is why the two ends cannot disagree -- one of them is the other's own build.

use std::path::Path;
use std::time::Duration;

use crate::remote::Endpoint;
use crate::remote::endpoint::peer_quote;
use crate::remote::transport::{ProtocolMismatch, Runner};

/// A runner for `endpoint`, greeted and speaking this build's protocol.
pub async fn runner(endpoint: &Endpoint, answer: Duration) -> anyhow::Result<Runner> {
    match endpoint {
        Endpoint::Local { .. } => anyhow::bail!("the local endpoint has no runner"),
        // A runner this machine starts is this build's own: there is nothing to choose.
        Endpoint::Direct { .. } => {
            return Runner::spawn(endpoint.runner_command()?, answer)
                .await
                .map_err(|e| endpoint.explain(e));
        }
        Endpoint::Remote { .. } => {}
    }
    // A named program is a preference; its greeting is what decides.
    let preferred = match endpoint.configured_runner() {
        Some(words) => match greet(endpoint, words, answer).await {
            Ok(runner) => return Ok(runner),
            Err(error) => Some(error.to_string()),
        },
        None => None,
    };
    let placed = place(endpoint).await?;
    match greet(endpoint, &placed, answer).await {
        Ok(runner) => Ok(runner),
        Err(error) => Err(anyhow::anyhow!(
            "the runner this build placed on the peer ({placed}) did not answer: {error}{}",
            match preferred {
                Some(why) => format!("; the configured one was refused first: {why}"),
                None => String::new(),
            }
        )),
    }
}

/// Starts one program on the peer and keeps it only if it greeted with this protocol.
async fn greet(endpoint: &Endpoint, words: &str, answer: Duration) -> anyhow::Result<Runner> {
    let command = endpoint.peer_command(&words.split_whitespace().collect::<Vec<_>>())?;
    match Runner::spawn(command, answer).await {
        Ok(runner) => Ok(runner),
        Err(error) => match error.downcast::<ProtocolMismatch>() {
            Ok(mismatch) => Err(mismatch.into()),
            Err(error) => Err(error),
        },
    }
}

/// Puts this build on the peer, and answers the words that run its runner.
///
/// # Invariant
///
/// The directory is named by this build, so a peer that already holds it is answered
/// without a transfer, and no other build's payload is ever touched. The runner and
/// the server go together because a channel's relay is the server, started beside the
/// runner that asked for it.
async fn place(endpoint: &Endpoint) -> anyhow::Result<String> {
    let (runner, server) = local_payload()?;
    let home = peer_home(endpoint).await?;
    let suffix = if is_dos(&home) { ".exe" } else { "" };
    let build = protocol::BUILD;
    let directory = format!("{}/.gongga/payload/{}", peer_quote(&home), build);
    let there = |name: &str| format!("{directory}/gongga-{name}{suffix}");
    // What `scp` and the peer's own sftp both resolve from the account's home.
    let sent = |name: &str| format!(".gongga/payload/{build}/gongga-{name}{suffix}");
    // Same name is not same bytes: compare sizes before running anything.
    let size = |path: &std::path::Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let present = run(
        endpoint,
        &format!(
            "test -f {runner} && test -f {server} && [ \"$(stat -c %s {runner})\" -eq {a} ] \
             && [ \"$(stat -c %s {server})\" -eq {b} ]",
            runner = there("runner"),
            server = there("server"),
            a = size(&runner),
            b = size(&server),
        ),
    )
    .await
    .is_some();
    if !present {
        run(endpoint, &format!("mkdir -p {directory}"))
            .await
            .ok_or_else(|| anyhow::anyhow!("the peer could not make {directory}"))?;
        copy(endpoint, &runner, &sent("runner")).await?;
        copy(endpoint, &server, &sent("server")).await?;
        run(
            endpoint,
            &format!("chmod +x {} {}", there("runner"), there("server")),
        )
        .await;
        tracing::info!(peer = %endpoint.describe(), directory = %directory, "payload placed");
    }
    // A payload program is one program, not a file that dispatches on its first word.
    Ok(there("runner"))
}

/// This build's own two programs: the runner, and the server a relay runs.
fn local_payload() -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    let me = std::env::current_exe().map_err(|e| anyhow::anyhow!("this program's path: {e}"))?;
    let directory = me
        .parent()
        .ok_or_else(|| anyhow::anyhow!("this program has no directory"))?;
    let beside =
        |name: &str| directory.join(format!("gongga-{name}{}", std::env::consts::EXE_SUFFIX));
    let runner = beside("runner");
    let server = beside("server");
    for path in [&runner, &server] {
        if !path.is_file() {
            anyhow::bail!(
                "this build cannot place itself on a peer: {} is not beside it",
                path.display()
            );
        }
    }
    Ok((runner, server))
}

/// Where the peer's own home is, in a spelling its shell can use again.
async fn peer_home(endpoint: &Endpoint) -> anyhow::Result<String> {
    const ASK: Duration = Duration::from_secs(30);
    let said = run(endpoint, "cd ~ && { pwd -W 2>/dev/null || pwd; }")
        .await
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the peer did not say where its home is, so this build cannot place its \
                 payload there ({ASK:?} is what one answer may take)"
            )
        })?;
    let text = said
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| anyhow::anyhow!("the peer answered with no home directory"))?;
    Ok(text.trim_end_matches('/').to_string())
}

/// Runs one script in the peer's own shell and answers what it printed, if it worked.
async fn run(endpoint: &Endpoint, script: &str) -> Option<String> {
    let mut command = endpoint.peer_command(&["bash", "-lc", script]).ok()?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(ONE_STEP, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        tracing::debug!(
            peer = %endpoint.describe(),
            script,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "the peer refused a placement step"
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Copies one file to the peer, at a path its own side resolves.
///
/// # Invariant
///
/// The exit status is the proof, with the runner's greeting after it: a copy that
/// failed leaves a file this build must not then try to run.
async fn copy(endpoint: &Endpoint, local: &Path, remote: &str) -> anyhow::Result<()> {
    let mut command = endpoint.copy_command(local, remote)?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(ONE_STEP, command.output())
        .await
        .map_err(|_| anyhow::anyhow!("copying {} to the peer took too long", local.display()))?
        .map_err(|e| anyhow::anyhow!("could not copy {} to the peer: {e}", local.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "the peer did not take {}: {}",
            local.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// How long one placement step may take: a copy of this build, not a command of anyone's.
const ONE_STEP: Duration = Duration::from_secs(300);

/// Whether a path the peer printed is a Windows one.
fn is_dos(path: &str) -> bool {
    path.len() > 2 && path.as_bytes()[1] == b':' && path.as_bytes()[0].is_ascii_alphabetic()
}
