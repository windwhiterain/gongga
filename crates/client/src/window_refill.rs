//! What a window asks for after a greeting, a scroll or a completed completion.
//!
//! See `docs/notes/client-refill.md`.

/// The window's own facts, read by a refill without naming a terminal.
///
/// # Invariant
///
/// These state what a frame drew, not requests the client sends: once the
/// journal lives here, this trait is implemented by the client itself.
pub trait WindowFacts {
    /// What a window's journal says it has drawn, and of which session.
    fn stamp_of(&self, window: &str) -> Option<(i64, i64)>;
    /// The oldest transcript row a window's journal says it holds.
    fn floor_of(&self, window: &str) -> Option<i64>;
    /// How many rows a pane could draw, as the last frame found it.
    fn room_of(&self, window: &str) -> usize;
    /// Records the rows a pane could draw: the only writer `room_of` has.
    ///
    /// # Invariant
    ///
    /// Only the drawing knows its own height, so this is the one fact a UI has to
    /// tell the client rather than the other way round. See `UiIntent::Room`.
    fn note_room(&mut self, window: &str, rows: usize);
    /// The windows a frame drew last.
    fn drawn(&self) -> &[String];
    /// The vocabulary changed; `version` names the new one so a UI can dedupe.
    fn refresh_completion(&mut self, version: u64);
}

/// One input line, handed to the command layer or sent as a message.
///
/// # Invariant
///
/// Sends what the completion asked for, and reports whether the popup is out of
/// date. It runs outside the key handler, because a completion is answered
/// synchronously and a peer's directory cannot be.
pub async fn answer_completion(
    client: &mut crate::Client,
    link: Option<&crate::Link>,
    ui: &mut dyn WindowFacts,
    seen: &mut u64,
) {
    if let Some(open) = link {
        for (target, directory) in client.take_wanted_paths() {
            let Some(message) = client.ask_paths(&target, &directory) else {
                continue;
            };
            if open.send(message).await.is_err() {
                break;
            }
        }
    }
    let version = client.paths_version();
    if version != *seen {
        *seen = version;
        ui.refresh_completion(version);
    }
}

/// Tells the client what this window's own journals hold, once per greeting.
///
/// # Invariant
///
/// Only what is written down: a window the journal says nothing about is left
/// undeclared, and an undeclared window is never fetched -- a fresh client has
/// nothing to be behind on. See `docs/notes/tui-run.md`.
pub fn declare(client: &mut crate::Client, ui: &dyn WindowFacts) {
    let names: Vec<String> = client.sessions().iter().map(|s| s.name.clone()).collect();
    for name in names {
        match ui.stamp_of(&name) {
            Some((created, drawn)) => {
                client.declare_drawn(&name, created, drawn, ui.floor_of(&name))
            }
            // No journal at all: nothing drawn, so the whole transcript is owed.
            None => client.declare_empty(&name),
        }
    }
}

/// Asks the server for the rows of the windows a frame drew and the greeting
/// found behind.
///
/// # Invariant
///
/// The draw is the trigger: a session nobody is reading is never fetched, so a
/// reconnect costs one request per window the operator is looking at rather than
/// one per session on the machine. The answer arrives as the events that drew
/// those rows the first time, so a refilled window is drawn by the code that drew
/// it live.
pub async fn answer_transcripts(
    client: &mut crate::Client,
    link: Option<&crate::Link>,
    ui: &dyn WindowFacts,
) {
    let Some(open) = link else {
        return;
    };
    let behind: Vec<String> = client.behind().into_iter().map(|(name, _)| name).collect();
    for name in behind {
        if !ui.drawn().iter().any(|drawn| *drawn == name) {
            continue;
        }
        let Some(message) = client.ask_transcript(&name, ui.room_of(&name)) else {
            continue;
        };
        if open.send(message.clone()).await.is_err() {
            client.never_sent(&message);
            break;
        }
    }
}

/// Asks the server for the page above what the windows the operator scrolled past
/// their first row hold.
///
/// # Invariant
///
/// The keystroke is the request: a page is fetched because someone asked to read
/// above the rows they had, so a window sitting at its oldest row is not refilled
/// over and over. `owed` is what the window asked for and has not been sent yet:
/// a page is asked once, and kept only while an answer is already on its way.
pub async fn answer_older(
    client: &mut crate::Client,
    link: Option<&crate::Link>,
    ui: &dyn WindowFacts,
    owed: &mut Vec<String>,
) {
    let Some(open) = link else {
        return;
    };
    for name in std::mem::take(owed) {
        // The transcript's own first row: there is nothing above it to read.
        if ui.floor_of(&name).is_some_and(|floor| floor <= 1) {
            continue;
        }
        // One page, shaped by the pane. A fill already on its way answers it too.
        match client.ask_older(&name, ui.room_of(&name)) {
            Some(message) => {
                if open.send(message.clone()).await.is_err() {
                    client.never_sent(&message);
                    break;
                }
            }
            None if client.waiting(&name) => owed.push(name),
            None => {}
        }
    }
}
