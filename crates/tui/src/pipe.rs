//! The terminal as a front-end of the client process.
//!
//! See `DESIGN.md` §1, "形态 2", and `docs/notes/client-pipe.md`.
//!
//! # Invariant
//!
//! The window owns the pipes and the drawing; the child owns the loop, the
//! journal and the link to the server. Closing stdin is the window going away,
//! and an ended stdout is the child going away. Neither polls the other's pid.

use anyhow::Context;
use client::render_wire::{RenderMessage, UiIntent, encode_intent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::mpsc;

use crate::{Tui, UiEvent, closing, run};

/// One intent on the child's stdin, flushed so a keystroke is not held back.
async fn send(input: &mut ChildStdin, intent: &UiIntent) -> anyhow::Result<()> {
    input.write_all(encode_intent(intent)?.as_bytes()).await?;
    input.flush().await?;
    Ok(())
}

/// How often a running call's clock is asked for a frame.
///
/// # Invariant
///
/// Shorter than the tenth of a second the duration prints, so no number is drawn
/// late, and asked for only while something runs (`Tui::is_running`): an idle window
/// still draws nothing it was not sent.
const CLOCK: std::time::Duration = std::time::Duration::from_millis(100);

/// Runs the window over the two pipes until either end goes away.
pub async fn run(args: &client::Args) -> anyhow::Result<()> {
    // A terminal that cannot open must not leave a child or a reader behind.
    let mut tui = Tui::for_pipe()?;
    let client::child::Child {
        mut process,
        mut input,
        output,
        log: _,
    } = client::child::spawn(args)?;

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
            // A running tool's clock is the one row that moves with no event.
            _ = tokio::time::sleep(CLOCK), if tui.is_running() => {
                tui.tick();
                if let Err(e) = tui.draw() {
                    break Err(e);
                }
            }
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
    let waited = process.wait().await.context("waiting for gongga-client");
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
