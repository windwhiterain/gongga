//! The session-list selection every front-end shares.
//!
//! # Invariant
//!
//! The front-ends draw their own lists and know which rows are on screen, but
//! the selection is one fact with one owner: this module keeps the anchor a
//! range spans from, and the command layer turns a click into the shown set.
//! See `docs/notes/webui-web.md`.

use crate::protocol::SelectGesture;

/// The session row the last non-range click named.
///
/// # Invariant
///
/// A view of this process, never written to disk: the shown set is the
/// persisted fact, and an anchor that outlives the window would make the first
/// range click of a fresh window reach for a row nobody anchored. A plain or
/// toggle click names it; a range click keeps it and starts one at its target
/// when none exists yet.
#[derive(Debug, Default)]
pub struct Selection {
    anchor: Option<String>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records what one click means for the next range.
    pub fn note(&mut self, target: &str, gesture: SelectGesture) {
        match gesture {
            SelectGesture::Plain | SelectGesture::Toggle => {
                self.anchor = Some(target.to_string());
            }
            // A range with no anchor starts one; any other range keeps the
            // anchor the earlier non-range click named.
            SelectGesture::Range if self.anchor.is_none() => {
                self.anchor = Some(target.to_string());
            }
            SelectGesture::Range => {}
        }
    }

    /// The visible session rows from the anchor through `target`, inclusive, in
    /// the order the list draws them.
    ///
    /// # Invariant
    ///
    /// `visible` holds session rows only: group rows name no session and a row
    /// inside a collapsed group is not on screen, so the caller has already left
    /// both out. When the anchor is absent or no longer visible, the span is the
    /// target alone -- the click starts a fresh selection rather than reaching
    /// across the list for a row the operator cannot see.
    pub fn span<'a>(&self, target: &str, visible: &'a [String]) -> Vec<&'a str> {
        let Some(anchor) = self.anchor.as_deref() else {
            return visible
                .iter()
                .find(|name| name.as_str() == target)
                .into_iter()
                .map(String::as_str)
                .collect();
        };
        let anchor_at = visible.iter().position(|name| name == anchor);
        let target_at = visible.iter().position(|name| name == target);
        let (Some(anchor_at), Some(target_at)) = (anchor_at, target_at) else {
            return visible
                .iter()
                .find(|name| name.as_str() == target)
                .into_iter()
                .map(String::as_str)
                .collect();
        };
        let (from, to) = (anchor_at.min(target_at), anchor_at.max(target_at) + 1);
        visible[from..to].iter().map(String::as_str).collect()
    }
}
