//! The client process a front-end starts: one pipe in, one pipe out.
//!
//! See `docs/notes/client-child.md`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::Args;

/// A started client, its two pipes, and the log its diagnostics go to.
pub struct Child {
    pub process: tokio::process::Child,
    pub input: ChildStdin,
    pub output: ChildStdout,
    pub log: PathBuf,
}

/// Starts the client beside this program, owned by the UI this process is.
///
/// # Invariant
///
/// `--parent` is this process's pid, so a server the client starts follows the UI and outlives
/// a crashed client; closing stdin is the window leaving and an ended stdout is the client
/// leaving. Neither side polls the other's pid. See `docs/notes/client-pipe.md`.
pub fn spawn(args: &Args) -> anyhow::Result<Child> {
    let client_home = crate::client_home_of(args);
    let log = client_home.join("client.log");
    let mut process = Command::new(program()?)
        .args(arguments(args))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr(&client_home))
        .kill_on_drop(true)
        .spawn()
        .context("starting gongga-client")?;
    let (Some(input), Some(output)) = (process.stdin.take(), process.stdout.take()) else {
        anyhow::bail!("gongga-client did not provide its pipes");
    };
    Ok(Child {
        process,
        input,
        output,
        log,
    })
}

/// The client travels beside the front-end: a bundle extracts both together.
fn program() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this program")?;
    let name = if cfg!(windows) {
        "gongga-client.exe"
    } else {
        "gongga-client"
    };
    let beside = exe.with_file_name(name);
    anyhow::ensure!(
        beside.is_file(),
        "gongga-client is not beside {}",
        exe.display()
    );
    Ok(beside)
}

/// Only the connection flags the child needs; a layer word is not one of them.
fn arguments(args: &Args) -> Vec<String> {
    let mut out = vec!["--parent".to_string(), std::process::id().to_string()];
    if let Some(value) = &args.conn.server {
        out.push("--server".to_string());
        out.push(value.clone());
    }
    if let Some(value) = &args.conn.home {
        out.push("--home".to_string());
        out.push(value.to_string_lossy().into_owned());
    }
    if let Some(value) = &args.conn.client_home {
        out.push("--client-home".to_string());
        out.push(value.to_string_lossy().into_owned());
    }
    if let Some(value) = &args.conn.key {
        out.push("--key".to_string());
        out.push(value.clone());
    }
    if args.client_only {
        out.push("--client_only".to_string());
    }
    if args.force {
        out.push("--force".to_string());
    }
    out
}

/// The child's diagnostics go to a file, never into the pipe that carries frames.
fn stderr(client_home: &Path) -> Stdio {
    let _ = std::fs::create_dir_all(client_home);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(client_home.join("client.log"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null())
}
