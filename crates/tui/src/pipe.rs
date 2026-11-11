//! The terminal as a front-end of the client process.
//!
//! See `DESIGN.md` §1, "形态 2", and `docs/notes/client-pipe.md`.
//!
//! # Invariant
//!
//! The window owns the pipes and the drawing; the child owns the loop, the
//! journal and the link to the server. Closing stdin is the window going away,
//! and an ended stdout is the child going away. Neither polls the other's pid.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use client::render_wire::{RenderMessage, UiIntent, encode_intent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc;

use crate::{Tui, UiEvent, closing, run};

/// The client travels beside this program: a bundle extracts both together.
fn client_program() -> anyhow::Result<PathBuf> {
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
fn client_args(args: &client::Args) -> Vec<String> {
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

/// The child's diagnostics go to a file, never into the alternate screen.
fn child_stderr(client_home: &Path) -> Stdio {
    let _ = std::fs::create_dir_all(client_home);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(client_home.join("client.log"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null())
}

/// One intent on the child's stdin, flushed so a keystroke is not held back.
async fn send(input: &mut ChildStdin, intent: &UiIntent) -> anyhow::Result<()> {
    input.write_all(encode_intent(intent)?.as_bytes()).await?;
    input.flush().await?;
    Ok(())
}

/// Runs the window over the two pipes until either end goes away.
pub async fn run(args: &client::Args) -> anyhow::Result<()> {
    let program = client_program()?;
    // A terminal that cannot open must not leave a child or a reader behind.
    let mut tui = Tui::for_pipe()?;
    let client_home = client::client_home_of(args);
    let mut child = Command::new(program)
        .args(client_args(args))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(child_stderr(&client_home))
        .kill_on_drop(true)
        .spawn()
        .context("starting gongga-client")?;
    let (Some(mut input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        child
            .kill()
            .await
            .context("stopping the client without pipes")?;
        anyhow::bail!("gongga-client did not provide its pipes");
    };

    // One reader owns stdout; the loop never holds two mutable borrows of the Tui.
    let (tx, mut rx) = mpsc::unbounded_channel::<anyhow::Result<RenderMessage>>();
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .context("reading the client's stdout")?
        {
            // A closed window still drains stdout until the child's writer exits.
            if !tx.is_closed() {
                let frame = serde_json::from_str::<RenderMessage>(&line)
                    .context("decoding a frame from the client's stdout");
                let _ = tx.send(frame);
            }
        }
        anyhow::Ok(())
    });

    let result = 'events: loop {
        tokio::select! {
            biased;
            _ = closing::asked() => break Ok(()),
            event = tui.next_event() => {
                let Some(event) = event else { break Ok(()); };
                if let UiEvent::Intent(intent) = event
                    && send(&mut input, &intent).await.is_err()
                {
                    break Ok(());
                }
            }
            frame = rx.recv() => {
                let Some(frame) = frame else { break Ok(()); };
                // New arrivals return through the keyboard branch before the next batch.
                let queued = rx.len();
                let frames = std::iter::once(frame)
                    .chain((0..queued).map_while(|_| rx.try_recv().ok()));
                for frame in frames {
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(error) => break 'events Err(error),
                    };
                    if let Some(intent) = run::apply_frame(&mut tui, frame)
                        && send(&mut input, &intent).await.is_err()
                    {
                        break 'events Ok(());
                    }
                }
                if let Err(e) = tui.draw() {
                    break Err(e);
                }
            }
        }
    };
    // EOF ends the child's loop, but its writer still needs stdout drained.
    drop(input);
    drop(rx);
    let restored = tui.restore();
    let waited = child.wait().await.context("waiting for gongga-client");
    reader.abort();
    let read = reader.await;
    result?;
    restored?;
    waited?;
    match read {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error.into()),
    }
}
