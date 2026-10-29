//! The channel-agnostic half: who may talk, which session, and what to reply.
//! See `docs/notes/bridge-chat.md`.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use protocol::ResponsePart;

use crate::channel::{ChatId, MsgRef};

/// How long a delivered message id is remembered; platforms retry deliveries.
const DEDUPE_WINDOW: Duration = Duration::from_secs(300);
/// Cap, so a long-running bridge cannot grow a set of ids forever.
const DEDUPE_CAP: usize = 4096;

/// Recently seen message ids; in memory only, so a restart may re-run one.
pub struct Dedupe {
    window: Duration,
    seen: HashMap<String, Instant>,
    order: VecDeque<String>,
}

impl Default for Dedupe {
    fn default() -> Self {
        Self::with_window(DEDUPE_WINDOW)
    }
}

impl Dedupe {
    pub fn with_window(window: Duration) -> Self {
        Self {
            window,
            seen: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// True the first time an id is offered, false on every retry.
    pub fn first_time(&mut self, id: &str) -> bool {
        self.prune();
        if self.seen.contains_key(id) {
            return false;
        }
        self.seen.insert(id.to_string(), Instant::now());
        self.order.push_back(id.to_string());
        while self.order.len() > DEDUPE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    fn prune(&mut self) {
        let now = Instant::now();
        // Insertion order is age order, so the front is always the oldest.
        while let Some(front) = self.order.front() {
            match self.seen.get(front) {
                Some(at) if now.duration_since(*at) < self.window => break,
                _ => {
                    let old = self.order.pop_front().unwrap();
                    self.seen.remove(&old);
                }
            }
        }
    }
}

/// A message handed to the harness that has not been answered yet.
#[derive(Debug, Clone)]
pub struct Pending {
    pub chat: ChatId,
    pub reply: MsgRef,
    /// The words handed to the harness, which the read-back window starts with.
    pub text: String,
}

/// What the bridge remembers about one conversation.
#[derive(Default)]
pub struct ChatState {
    /// Where the transcript stood when the last answer was delivered.
    ///
    /// # Invariant
    ///
    /// `None` until the first message, and the transcript is never written to
    /// before that read: otherwise the read-back replays the whole history and
    /// relays an old answer as if it were new.
    pub last_seq: Option<i64>,
    /// Messages handed over and unanswered, oldest first: the server's order.
    pub waiting: VecDeque<Pending>,
}

/// One row of a transcript read-back: a response part, without the session the
/// read was already about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The store's own word for the row, which the part it arrived as says.
    pub kind: &'static str,
    pub text: String,
}

impl Row {
    /// The row this streamed part was stored as.
    pub fn of(part: ResponsePart, text: &str) -> Self {
        Self {
            kind: match part {
                ResponsePart::Output => "text",
                ResponsePart::Reasoning => "reasoning",
                ResponsePart::ToolCall => "tool_call",
                ResponsePart::Activity => "builtin_call",
            },
            text: text.to_string(),
        }
    }
}

/// Picks the sentence a chat user should receive from one turn's rows.
///
/// # Invariant
///
/// Only the last assistant text is relayed, and the message the bridge handed
/// over is never its own answer: the read window starts at that message and the
/// store sends oldest first, which is the order the channel keeps.
pub fn final_reply(rows: &[Row], sent: &str) -> String {
    for row in rows.iter().rev() {
        let text = row.text.trim();
        if row.kind == "text" && !text.is_empty() && text != sent {
            return text.to_string();
        }
    }
    // A silent turn still needs a sentence; silence reads as a crash.
    if rows.iter().any(|row| row.kind == "tool_call") {
        return "跑完了，但只调用了工具，没有文字回答。".to_string();
    }
    "跑完了，但没有任何输出。".to_string()
}

/// Truncates to `max_chars` **characters**, then says so.
///
/// # Invariant
///
/// The cut lands on a character boundary, never a byte index, and a shortened
/// reply always says so: a silent cut reads like an agent that stopped.
pub fn clip(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len().min(max_chars * 4));
    for (count, ch) in text.chars().enumerate() {
        if count == max_chars {
            out.push_str("\n…（太长，已截断；完整回答在会话 transcript 里）");
            return out;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &'static str, text: &str) -> Row {
        Row {
            kind,
            text: text.to_string(),
        }
    }

    /// A retried delivery is the normal case: each retry would be a new turn.
    #[test]
    fn the_same_message_id_is_only_accepted_once() {
        let mut dedupe = Dedupe::default();
        assert!(dedupe.first_time("m1"));
        assert!(
            !dedupe.first_time("m1"),
            "second delivery is the same message"
        );
        assert!(dedupe.first_time("m2"));
    }

    #[test]
    fn ids_are_forgotten_after_the_window() {
        let mut dedupe = Dedupe::with_window(Duration::from_millis(20));
        assert!(dedupe.first_time("m1"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            dedupe.first_time("m1"),
            "a reused id is a new message after the window"
        );
    }

    #[test]
    fn the_bounded_set_drops_the_oldest() {
        let mut dedupe = Dedupe::with_window(Duration::from_secs(60));
        for i in 0..(DEDUPE_CAP + 10) {
            dedupe.first_time(&format!("m{i}"));
        }
        assert!(dedupe.seen.len() <= DEDUPE_CAP);
        assert!(
            !dedupe.seen.contains_key("m0"),
            "oldest ids are evicted first"
        );
    }

    /// The last thing the model said is the answer; its errands are not.
    #[test]
    fn the_last_assistant_text_wins() {
        let rows = vec![
            row("text", "run the tests"),
            row("reasoning", "thinking about it"),
            row("text", "let me run them"),
            row("tool_call", r#"{"cmd":"cargo test"}"#),
            row("text", "65 passed, nothing broken."),
        ];
        assert_eq!(
            final_reply(&rows, "run the tests"),
            "65 passed, nothing broken."
        );
    }

    /// A turn that only ran a command has nothing to quote; saying nothing at
    /// all would look like the bridge died.
    #[test]
    fn a_silent_turn_explains_itself() {
        let rows = vec![
            row("text", "build it"),
            row("tool_call", r#"{"cmd":"cargo build"}"#),
        ];
        let reply = final_reply(&rows, "build it");
        assert!(reply.contains("工具"), "{reply}");
    }

    /// Echoing the question back is the one reply a chat user never deserves.
    #[test]
    fn the_users_message_is_not_the_reply() {
        let rows = vec![row("text", "hello there")];
        assert_ne!(final_reply(&rows, "hello there"), "hello there");
    }

    /// A read re-sends a row as the part it was streamed as, which is how the
    /// bridge tells an errand from an answer.
    #[test]
    fn a_response_part_is_the_row_it_was_stored_as() {
        assert_eq!(Row::of(ResponsePart::Output, "hi"), row("text", "hi"));
        assert_eq!(
            Row::of(ResponsePart::Reasoning, "hm"),
            row("reasoning", "hm")
        );
        assert_eq!(
            Row::of(ResponsePart::ToolCall, r#"{"cmd":"ls"}"#),
            row("tool_call", r#"{"cmd":"ls"}"#)
        );
        assert_eq!(
            Row::of(ResponsePart::Activity, "cited"),
            row("builtin_call", "cited")
        );
    }

    #[test]
    fn clipping_counts_characters_not_bytes() {
        let text = "一二三四五";
        let cut = clip(text, 3);
        assert!(cut.starts_with("一二三"), "{cut}");
        assert!(cut.contains("截断"), "{cut}");
        // Well inside the budget: untouched.
        assert_eq!(clip(text, 10), text);
    }

    #[test]
    fn clipping_to_zero_disables_it() {
        let text = "一二三四五";
        assert_eq!(clip(text, 0), text);
    }
}
