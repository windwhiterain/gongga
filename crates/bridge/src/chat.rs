//! The channel-agnostic half: who may talk, which session, and what to reply.
//! See `docs/notes/bridge-chat.md`.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

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

/// One row of a transcript read-back: a `message` event without the session the read
/// was already about.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Row {
    pub seq: i64,
    pub kind: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub text: String,
}

impl From<protocol::MessageEvent> for Row {
    fn from(row: protocol::MessageEvent) -> Self {
        Self {
            seq: row.seq,
            kind: row.kind,
            role: row.role,
            name: row.name,
            text: row.text,
        }
    }
}

/// The rows this bridge still owes a reply for: those after its cursor.
///
/// # Invariant
///
/// A read-back window can reach an earlier turn, whose answer is not this one.
pub fn since(rows: &[Row], cursor: i64) -> Vec<Row> {
    rows.iter()
        .filter(|row| row.seq > cursor)
        .cloned()
        .collect()
}

/// Picks the sentence a chat user should receive from one turn's rows.
///
/// # Invariant
///
/// Only the last assistant text is relayed; a turn's earlier "let me look"
/// lines are never concatenated in, or the bot reads as talking to itself.
pub fn final_reply(rows: &[Row]) -> String {
    // Sort by seq: "the last thing it said" is a claim about time, not wire order.
    let mut ordered: Vec<&Row> = rows.iter().collect();
    ordered.sort_by_key(|row| row.seq);

    let mut last_tool: Option<&str> = None;
    let mut said: Vec<&str> = Vec::new();
    for row in ordered {
        match row.kind.as_str() {
            "tool_call" => last_tool = row.name.as_deref(),
            "text" if row.role.as_deref() == Some("assistant") => {
                let text = row.text.trim();
                if !text.is_empty() {
                    said.push(text);
                }
            }
            _ => {}
        }
    }
    if let Some(text) = said.last() {
        return (*text).to_string();
    }
    // A silent turn still needs a sentence; silence reads as a crash.
    match last_tool {
        Some(tool) => format!("跑完了，但只调用了工具 `{tool}`，没有文字回答。"),
        None => "跑完了，但没有任何输出。".to_string(),
    }
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

    fn row(seq: i64, kind: &str, role: Option<&str>, name: Option<&str>, text: &str) -> Row {
        Row {
            seq,
            kind: kind.to_string(),
            role: role.map(str::to_string),
            name: name.map(str::to_string),
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

    #[test]
    fn the_last_assistant_text_wins() {
        let rows = vec![
            row(1, "text", Some("user"), None, "run the tests"),
            row(2, "reasoning", None, None, "thinking about it"),
            row(3, "text", Some("assistant"), None, "let me run them"),
            row(
                4,
                "tool_call",
                None,
                Some("bash"),
                r#"{"cmd":"cargo test"}"#,
            ),
            row(5, "tool_result", None, None, "ok. 65 passed"),
            row(
                6,
                "text",
                Some("assistant"),
                None,
                "65 passed, nothing broken.",
            ),
        ];
        assert_eq!(final_reply(&rows), "65 passed, nothing broken.");
    }

    /// A turn that only ran a command has nothing to quote; saying nothing at
    /// all would look like the bridge died.
    #[test]
    fn a_silent_turn_explains_itself() {
        let rows = vec![
            row(1, "text", Some("user"), None, "build it"),
            row(
                2,
                "tool_call",
                None,
                Some("bash"),
                r#"{"cmd":"cargo build"}"#,
            ),
            row(3, "tool_result", None, None, "ok"),
        ];
        let reply = final_reply(&rows);
        assert!(reply.contains("bash"), "{reply}");
    }

    /// A tool-only turn must not relay the previous answer as its own.
    #[test]
    fn a_silent_turn_does_not_repeat_the_earlier_answer() {
        let rows = vec![
            row(1, "text", Some("assistant"), None, "the earlier answer"),
            row(2, "text", Some("user"), None, "check it"),
            row(3, "tool_call", None, Some("bash"), r#"{"cmd":"ls"}"#),
            row(4, "tool_result", None, None, "a.txt"),
        ];
        let fresh = since(&rows, 2);
        assert_eq!(fresh.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3, 4]);
        // What the user is owed is an explanation, not the previous answer again.
        assert!(
            final_reply(&fresh).contains("bash"),
            "{}",
            final_reply(&fresh)
        );
        assert_eq!(final_reply(&rows), "the earlier answer");
    }

    /// A turn that does answer relays its own words, filtered or not.
    #[test]
    fn rows_before_the_cursor_are_dropped() {
        let rows = vec![
            row(1, "text", Some("assistant"), None, "the earlier answer"),
            row(2, "text", Some("user"), None, "do it again"),
            row(3, "text", Some("assistant"), None, "this turn's answer"),
        ];
        let fresh = since(&rows, 2);
        assert_eq!(fresh.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3]);
        assert_eq!(final_reply(&fresh), "this turn's answer");
    }

    /// A cursor at the tail means the turn said nothing yet, not "send it all".
    #[test]
    fn a_cursor_at_the_tail_leaves_nothing_owed() {
        let rows = vec![row(1, "text", Some("assistant"), None, "old")];
        assert!(since(&rows, 1).is_empty());
        assert!(since(&[], 0).is_empty());
        // A silent turn still gets a sentence rather than an empty message.
        assert!(!final_reply(&since(&rows, 1)).is_empty());
    }

    /// The user's own words must never be echoed back as the answer.
    #[test]
    fn the_users_message_is_not_the_reply() {
        let rows = vec![row(1, "text", Some("user"), None, "hello there")];
        assert_ne!(final_reply(&rows), "hello there");
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
