//! What one frame costs as the log grows. See `docs/notes/tui-frame_cost.md`.

use std::time::Instant;

use gongga_tui::{LineKind, Log, View};

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
        log.rows(View::All, width);

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
            std::hint::black_box(log.window(View::All, width, height, 0));
        });
        let total = log.rows(View::All, width);
        let scrolled = time(&mut || {
            std::hint::black_box(log.window(View::All, width, height, total / 2));
        });

        println!(
            "{count:>6} lines: old {old:>12?}   new {pinned:>9?}   new, scrolled {scrolled:>9?}"
        );
    }
}
