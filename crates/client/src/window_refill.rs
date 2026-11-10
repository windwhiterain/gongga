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
    /// The rows this window's journal holds above `before`, newest first.
    ///
    /// # Invariant
    ///
    /// The client's copy of the server's state, read back by number: a page comes
    /// from here first, and from the server only when the journal has nothing above
    /// `before`. See `docs/notes/client-window-journal.md`.
    fn events(&self, window: &str, before: u64, count: usize) -> Vec<(u64, protocol::Event)>;
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
    ui: &mut dyn crate::runtime::Frontend,
) {
    let Some(open) = link else {
        return;
    };
    let behind: Vec<String> = client.behind().into_iter().map(|(name, _)| name).collect();
    for name in behind {
        if !ui.drawn().iter().any(|drawn| *drawn == name) {
            continue;
        }
        // The copy on disk is what the window shows first: a client that kept the
        // server's state does not ask for rows it can already draw. Only the rows
        // it is short of go over the wire.
        let count = ui.room_of(&name).max(1).saturating_mul(16).clamp(16, 600);
        let mut held = ui.events(&name, u64::MAX, count);
        if !held.is_empty() {
            held.reverse();
            let mut sink = ui.sink();
            for (_, event) in held {
                client.handle_event(event, &mut *sink);
            }
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
    ui: &mut dyn crate::runtime::Frontend,
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
        // The local copy first: the rows above the oldest this window holds are in
        // the journal, numbered. Replayed by the code that drew them the first time,
        // so a page read from disk is the page the server would have sent.
        if let Some(floor) = ui.floor_of(&name).map(|floor| floor.max(1) as u64) {
            let count = ui.room_of(&name).max(1).saturating_mul(16).clamp(16, 600);
            let mut held = ui.events(&name, floor, count);
            // Only the run that touches the window: a row behind a gap is the
            // copy's, not this page's.
            let mut touch = floor.saturating_sub(1);
            held.retain(|(seq, _)| {
                let keep = *seq == touch;
                if keep {
                    touch = touch.saturating_sub(1);
                }
                keep
            });
            if !held.is_empty() {
                held.reverse();
                let start = held[0].0;
                let mut page = crate::Page::default();
                for (_, event) in held {
                    client.handle_event(event, &mut page);
                }
                if !page.rows.is_empty() {
                    let (created, drawn) = ui.stamp_of(&name).unwrap_or((0, 0));
                    let mut sink = ui.sink();
                    sink.older_page(&name, &page.rows);
                    sink.landed(&name, created, drawn, start as i64);
                    continue;
                }
            }
        }
        // Anchored at the row the window is short of; the pane's height is only
        // how much history comes back with it. A fill already on its way answers
        // it too.
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
