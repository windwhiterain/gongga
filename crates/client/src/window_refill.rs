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
/// A cached generation or range the server no longer has must not be rendered.
pub fn declare(client: &mut crate::Client, ui: &mut dyn crate::runtime::Frontend) {
    let invalid: Vec<String> = client
        .sessions()
        .iter()
        .filter_map(|session| {
            ui.stamp_of(&session.name)
                .filter(|(created, drawn)| {
                    *created != session.created_at || *drawn > session.transcript
                })
                .map(|_| session.name.clone())
        })
        .collect();
    for name in invalid {
        ui.sink().forget(&name);
        client.declare_empty(&name);
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
    for name in ui.drawn().to_vec() {
        if client.needs_restore(&name)
            && let Some((created, drawn)) = ui.stamp_of(&name)
            && drawn >= client.visible_floor(&name, ui.room_of(&name))
        {
            let first = client.visible_floor(&name, ui.room_of(&name));
            let mut held = ui.events(
                &name,
                (drawn.max(0) as u64).saturating_add(1),
                ui.room_of(&name).max(1),
            );
            held.retain(|(seq, _)| *seq >= first as u64);
            held.reverse();
            if let Some((floor, _)) = held.first() {
                let floor = *floor as i64;
                {
                    let mut sink = ui.sink();
                    sink.clear(&name);
                    for (_, event) in held {
                        client.handle_event(event, &mut *sink);
                    }
                }
                client.declare_drawn(&name, created, drawn, Some(floor));
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
    let names: std::collections::BTreeSet<String> = std::mem::take(owed).into_iter().collect();
    for name in names {
        if client.waiting(&name) {
            owed.push(name);
            continue;
        }
        let Some(floor) = client.drawn_floor(&name) else {
            continue;
        };
        if floor <= 1 {
            continue;
        }
        if let Some(((created, through), first)) = ui.stamp_of(&name).zip(ui.floor_of(&name))
            && through >= floor - 1
            && first < floor
        {
            let held = ui.events(&name, floor as u64, ui.room_of(&name).max(1));
            let mut held: Vec<_> = held
                .into_iter()
                .filter(|(seq, _)| *seq >= first as u64)
                .collect();
            held.reverse();
            if let Some((start, _)) = held.first() {
                let start = *start as i64;
                let mut page = crate::Page::default();
                for (_, event) in held {
                    client.handle_event(event, &mut page);
                }
                ui.sink().older_page(&name, &page.rows);
                client.extend_drawn(&name, created, start, floor - 1);
                continue;
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
