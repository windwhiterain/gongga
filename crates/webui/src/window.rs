//! One page is one window: it owns a client child while the page is attached.
//!
//! See `docs/notes/webui-window.md`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use client::child;

use crate::server::State;

/// Why a page is closed; the browser shows this reason.
const CLOSE_CODE: u16 = 4001;
/// A close reason holds 123 bytes, and one client log line can be longer.
const REASON_BYTES: usize = 120;
/// How much of the log's end is read for the last word.
const TAIL_BYTES: u64 = 8 * 1024;

/// Serves one attached page until either end goes away.
pub async fn attach(io: TokioIo<Upgraded>, state: Arc<State>) {
    let websocket = WebSocketStream::from_raw_socket(io, Role::Server, None).await;
    if !state.slot.claim() {
        close(
            websocket,
            "another page is attached; close it, or start gongga-webui --instance <name>",
        )
        .await;
        return;
    }
    if let Err(error) = run(websocket, state.clone()).await {
        eprintln!("gongga-webui: a window ended in error: {error}");
    }
    state.slot.release();
}

/// Who ended a window: the page, the client, or this process leaving.
enum Ending {
    PageGone,
    ClientGone,
    WindowClosed,
}

async fn run(
    websocket: WebSocketStream<TokioIo<Upgraded>>,
    state: Arc<State>,
) -> anyhow::Result<()> {
    // A page that attaches while this process is leaving never owns a child.
    let mut shutdown = state.shutdown();
    if *shutdown.borrow_and_update() {
        close(websocket, "gongga-webui is leaving").await;
        return Ok(());
    }
    let started = child::spawn(&state.client);
    let child = match started {
        Ok(child) => child,
        Err(error) => {
            close(websocket, &format!("gongga-client did not start: {error}")).await;
            return Ok(());
        }
    };
    let child::Child {
        mut process,
        mut input,
        output,
        log,
    } = child;
    let (mut sink, mut stream) = websocket.split();
    let mut lines = BufReader::new(output).lines();
    let ending = loop {
        tokio::select! {
            _ = shutdown.changed() => break Ending::WindowClosed,
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if sink.send(Message::Text(line)).await.is_err() {
                        break Ending::PageGone;
                    }
                }
                Ok(None) => break Ending::ClientGone,
                Err(error) => {
                    eprintln!("gongga-webui: reading the client's stdout: {error}");
                    break Ending::ClientGone;
                }
            },
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if !write_intent(&mut input, &text).await {
                        break Ending::ClientGone;
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if sink.send(Message::Pong(payload)).await.is_err() {
                        break Ending::PageGone;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ending::PageGone,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    eprintln!("gongga-webui: reading the page's socket: {error}");
                    break Ending::PageGone;
                }
            },
        }
    };
    // Closing stdin is the window leaving; a reader stays on stdout so the child's
    // writer never waits on a full pipe.
    drop(input);
    let drain = tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    let reason = match &ending {
        Ending::ClientGone => tail(&log),
        Ending::WindowClosed => "gongga-webui is leaving".to_string(),
        Ending::PageGone => "the page closed its window".to_string(),
    };
    let _ = sink.send(Message::Close(Some(frame(&reason)))).await;
    let _ = sink.flush().await;
    drop(sink);
    let _ = process.wait().await;
    drain.abort();
    Ok(())
}

/// One intent on the child's stdin, flushed so a key is never held back.
async fn write_intent(input: &mut tokio::process::ChildStdin, text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    if input.write_all(text.as_bytes()).await.is_err() {
        return false;
    }
    if input.write_all(b"\n").await.is_err() {
        return false;
    }
    input.flush().await.is_ok()
}

/// Closes a socket this process still owns, with the reason a browser can show.
async fn close(mut websocket: WebSocketStream<TokioIo<Upgraded>>, reason: &str) {
    let _ = websocket.close(Some(frame(reason))).await;
}

/// The close frame, cut to what a frame can carry on a character boundary.
fn frame(reason: &str) -> CloseFrame<'static> {
    let mut end = 0;
    for (at, character) in reason.char_indices() {
        let next = at + character.len_utf8();
        if next > REASON_BYTES {
            break;
        }
        end = next;
    }
    CloseFrame {
        code: CloseCode::Library(CLOSE_CODE),
        reason: reason[..end].to_string().into(),
    }
}

/// The client's last word: the log a page with no frame between was closed with.
fn tail(log: &Path) -> String {
    let ended = || "gongga-client ended".to_string();
    let mut file = match std::fs::File::open(log) {
        Ok(file) => file,
        Err(_) => return ended(),
    };
    let size = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => 0,
    };
    if file
        .seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .is_err()
    {
        return ended();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return ended();
    }
    match text.lines().rev().find(|line| !line.trim().is_empty()) {
        Some(line) => line.trim().to_string(),
        None => ended(),
    }
}
