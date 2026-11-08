//! Window state sync and command bookkeeping, without a terminal.
//!
//! See `docs/notes/client-window-sync.md`.

use crate::Client;
use crate::window_command as command;
use crate::window_state::{Follow, State};

/// What a front-end does when the window's own state changes.
///
/// # Invariant
///
/// The front-end owns its sink: `refresh_lines` builds the sink itself, so the
/// client never borrows a terminal type. A later client-side journal implements
/// the other half locally.
pub trait WindowView {
    /// Show the settings the operator keeps in the state file.
    fn show_state(&mut self, state: &State);
    /// The completion's own vocabulary changed.
    fn refresh_completion(&mut self);
    /// Draw the lines a refresh produced, with this front-end's own sink.
    fn refresh_lines(&mut self, client: &Client);
    /// The commands the server answered since the last frame.
    fn take_settled(&mut self) -> Vec<(u64, bool)>;
}

/// Tells the view and the completion what the window's own state is now.
///
/// # Invariant
///
/// One place sets all of it, after any change: the block size, the margin, the
/// sticky session, the sessions drawn beside the list, and the completion's own
/// vocabulary. A front-end that set half of it would draw one state and complete
/// against another.
pub fn sync(
    client: &mut Client,
    state: &State,
    window: &command::Window,
    view: &mut dyn WindowView,
) {
    view.show_state(state);
    client.set_sticky_vocabulary(&state.sticky().names(), state.sticky().get("session"));
    client.set_shown(state.shown_names());
    window.refresh(client, state);
    view.refresh_completion();
    view.refresh_lines(client);
}

/// Applies what the window kept for the commands the server has now answered.
///
/// # Invariant
///
/// Only an *accepted* command writes its entries back: a refusal leaves the file
/// as it was, which is why nothing was written when the line was sent.
pub fn settle(
    client: &Client,
    state: &mut State,
    pending: &mut Vec<(u64, Follow)>,
    view: &mut dyn WindowView,
) {
    for (id, ok) in view.take_settled() {
        let Some(at) = pending.iter().position(|(other, _)| *other == id) else {
            continue;
        };
        let (_, pairs) = pending.remove(at);
        if !ok {
            continue;
        }
        let written: Vec<String> = pairs.iter().map(|(kind, _)| kind.clone()).collect();
        let mut changed: Vec<String> = Vec::new();
        for (kind, value) in &pairs {
            if state.change_sticky(kind, value).unwrap_or(false) {
                changed.push(kind.clone());
            }
        }
        // SPEC's `sticky to xxx`: what follows a changed model is forgotten, unless
        // this command stated it too.
        for (kind, parent) in command::follows(client) {
            if changed.contains(&parent) && !written.contains(&kind) {
                let _ = state.del_sticky(&kind);
            }
        }
    }
}
