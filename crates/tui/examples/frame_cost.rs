//! What one frame costs as the log grows. See `docs/notes/tui-frame_cost.md`.

use std::time::Instant;

use gongga_tui::{LineKind, Log};

/// The renderer as it was: wrap everything, allocate every row, then slice.
fn old_frame(lines: &[String], width: usize, height: usize) -> usize {
    fn wrap(text: &str, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        for hard in text.split('\n') {
            if hard.is_empty() {
                out.push(String::new());
                continue;
            }
            let mut current = String::new();
            let mut column = 0usize;
            for ch in hard.chars() {
                // The original measured a char by formatting it: one allocation each.
                let w = ch.to_string().len().max(1);
                if column + w > width && !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    column = 0;
                }
                current.push(ch);
                column += w;
            }
            out.push(current);
        }
        out
    }

    let mut wrapped = Vec::new();
    for line in lines {
        for segment in wrap(line, width) {
            wrapped.push(segment);
        }
    }
    let total = wrapped.len();
    let offset = total.saturating_sub(height);
    let visible = &wrapped[offset..(offset + height).min(total)];
    visible.iter().map(|s| s.len()).sum()
}

fn main() {
    let width = 80;
    let height = 24;
    let frames = 200;
    for count in [100usize, 1_000, 5_000, 20_000] {
        let texts: Vec<String> = (0..count)
            .map(|i| format!("line {i}: a plausible amount of text, long enough to wrap sometimes"))
            .collect();
        let mut log = Log::new();
        for text in &texts {
            log.push("w", LineKind::Assistant, text.clone());
        }
        log.rows("w", width);

        let time = |f: &mut dyn FnMut()| {
            let start = Instant::now();
            for _ in 0..frames {
                f();
            }
            start.elapsed() / frames
        };

        let old = time(&mut || {
            std::hint::black_box(old_frame(&texts, width, height));
        });
        let pinned = time(&mut || {
            std::hint::black_box(log.window("w", width, height, 0));
        });
        let total = log.rows("w", width);
        let scrolled = time(&mut || {
            std::hint::black_box(log.window("w", width, height, total / 2));
        });

        println!(
            "{count:>6} lines: old {old:>12?}   new {pinned:>9?}   new, scrolled {scrolled:>9?}"
        );
    }
    frame_of_two_panes(width, height);
}

/// One frame as the window draws it: the sessions' panes, then the command line.
///
/// # Invariant
///
/// The columns above are one window read at one width, which a frame is not: the
/// command area is as wide as the window and a session's pane is not, so a log with
/// one width for all of them lays itself out once per pane and per frame.
///
/// See `docs/notes/tui-frame_cost.md`.
fn frame_of_two_panes(width: usize, height: usize) {
    let frames = 100;
    for count in [100usize, 1_000, 5_000, 20_000] {
        let mut log = Log::new();
        for i in 0..count {
            log.push(
                "s",
                LineKind::Assistant,
                format!("line {i}: a plausible amount of text, long enough to wrap sometimes"),
            );
        }
        log.push(
            "cmd",
            LineKind::Assistant,
            "set_session --name a".to_string(),
        );
        // Both panes measured once, as they are when the operator types.
        log.rows("s", width / 2);
        log.rows("cmd", width);

        let start = Instant::now();
        for _ in 0..frames {
            std::hint::black_box(log.window("s", width / 2, height, 0));
            std::hint::black_box(log.window("cmd", width, height, 0));
        }
        println!(
            "{count:>6} lines: one frame, two widths {:>9?}",
            start.elapsed() / frames
        );
    }
}
