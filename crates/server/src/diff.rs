//! How much a text changed, and the patch that says so.
//!
//! See `docs/notes/server-diff.md`.

use std::collections::HashMap;
use std::ops::Range;

/// How many unchanged lines a patch repeats around each change.
const CONTEXT: usize = 3;

/// One line of a patch, in the order the text reads once the patch is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line<'a> {
    Kept(&'a str),
    Removed(&'a str),
    Added(&'a str),
}

/// The patch that turns `old` into `new`, or `None` when the change is too big.
///
/// # Invariant
///
/// A patch is returned only when applying it to `old` reproduces `new` exactly: a
/// diff that does not is dropped rather than appended, because a model reading it
/// would act on text this machine does not hold.
pub fn patch(old: &str, new: &str, tolerance: f64) -> Option<String> {
    let before = lines(old);
    let after = lines(new);
    let script = script(&before, &after);
    let (mut removed, mut added) = (0, 0);
    for line in &script {
        match line {
            Line::Kept(_) => {}
            Line::Removed(_) => removed += 1,
            Line::Added(_) => added += 1,
        }
    }
    // One edited line is one line: the change is the larger side, not the sum.
    if removed.max(added) as f64 >= tolerance.max(0.0) * before.len() as f64 {
        return None;
    }
    if !reproduces(&script, &after) {
        tracing::warn!(
            "a computed patch does not reproduce the text it was measured against; dropped"
        );
        return None;
    }
    Some(render(&script))
}

/// A text's lines, without the empty one a final newline leaves behind.
fn lines(text: &str) -> Vec<&str> {
    let mut out: Vec<&str> = text.split('\n').collect();
    if out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out
}

/// What became of every line, oldest first.
///
/// # Invariant
///
/// Built by halving the ranges at a line both sides still share, so the work is
/// proportional to the number of changes: a text rewritten end to end costs one
/// comparison per line, and a stack stands in for the recursion a long file would
/// otherwise spend stack on.
fn script<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Line<'a>> {
    let mut out: Vec<Line<'a>> = Vec::with_capacity(old.len() + new.len());
    let mut pending: Vec<Segment> = vec![Segment::Differ(0..old.len(), 0..new.len())];
    while let Some(segment) = pending.pop() {
        match segment {
            Segment::Kept(at) => out.push(Line::Kept(old[at])),
            Segment::Replace(o, n) => {
                out.extend(o.map(|at| Line::Removed(old[at])));
                out.extend(n.map(|at| Line::Added(new[at])));
            }
            Segment::Differ(o, n) => {
                // The head both sides still share reads as it stands.
                let mut head = 0;
                while o.start + head < o.end
                    && n.start + head < n.end
                    && old[o.start + head] == new[n.start + head]
                {
                    out.push(Line::Kept(old[o.start + head]));
                    head += 1;
                }
                let (mut oe, mut ne) = (o.end, n.end);
                while oe > o.start + head && ne > n.start + head && old[oe - 1] == new[ne - 1] {
                    oe -= 1;
                    ne -= 1;
                }
                // The untouched end is pushed first, so it comes out last.
                pending.extend((oe..o.end).rev().map(Segment::Kept));
                let (from_old, from_new) = (o.start + head, n.start + head);
                match anchor(old, new, from_old..oe, from_new..ne) {
                    Some((at, then)) => {
                        pending.push(Segment::Differ(at + 1..oe, then + 1..ne));
                        pending.push(Segment::Kept(at));
                        pending.push(Segment::Differ(from_old..at, from_new..then));
                    }
                    None => pending.push(Segment::Replace(from_old..oe, from_new..ne)),
                }
            }
        }
    }
    out
}

/// One range of two texts to compare, in half-open line indices.
#[derive(Debug, Clone)]
enum Segment {
    Differ(Range<usize>, Range<usize>),
    Replace(Range<usize>, Range<usize>),
    Kept(usize),
}

/// A line the two ranges share, occurring once on each side, near the middle of them.
///
/// # Invariant
///
/// Only a line unique to both ranges may anchor: an ambiguous one could pair the
/// wrong occurrence and claim a share that the text does not have. The picks stay in
/// order, so the two halves hold no crossing.
fn anchor(
    old: &[&str],
    new: &[&str],
    o: std::ops::Range<usize>,
    n: std::ops::Range<usize>,
) -> Option<(usize, usize)> {
    if o.is_empty() || n.is_empty() {
        return None;
    }
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for line in &old[o.clone()] {
        counts.entry(line).or_default().0 += 1;
    }
    for line in &new[n.clone()] {
        counts.entry(line).or_default().1 += 1;
    }
    let mut once: HashMap<&str, usize> = HashMap::new();
    for at in n.clone() {
        if counts[new[at]].1 == 1 {
            once.insert(new[at], at);
        }
    }
    let middle = (o.start + o.end) / 2;
    let mut chosen: Option<(usize, usize)> = None;
    let mut reached = 0;
    for at in o.clone() {
        if counts[old[at]].0 != 1 {
            continue;
        }
        let Some(&then) = once.get(old[at]) else {
            continue;
        };
        if then < reached {
            continue;
        }
        reached = then;
        let closer = chosen.is_none_or(|(best, _)| at.abs_diff(middle) < best.abs_diff(middle));
        if closer {
            chosen = Some((at, then));
        }
    }
    chosen
}

/// Whether the patch puts `after` back together: what makes an appended diff safe.
fn reproduces(script: &[Line], after: &[&str]) -> bool {
    let mut at = 0;
    for line in script {
        match line {
            Line::Removed(_) => {}
            Line::Kept(text) | Line::Added(text) => {
                if after.get(at) != Some(text) {
                    return false;
                }
                at += 1;
            }
        }
    }
    at == after.len()
}

/// The patch text: one block of hunks per region that changed.
fn render(script: &[Line]) -> String {
    let mut out = String::new();
    let places = places(script);
    for (start, end) in hunks(script) {
        let mut old_count = 0;
        let mut new_count = 0;
        for line in &script[start..end] {
            match line {
                Line::Kept(_) => {
                    old_count += 1;
                    new_count += 1;
                }
                Line::Removed(_) => old_count += 1,
                Line::Added(_) => new_count += 1,
            }
        }
        let (old_at, new_at) = places[start];
        // A side a hunk only adds to is anchored on the line before it.
        let old_shown = if old_count == 0 {
            old_at.saturating_sub(1)
        } else {
            old_at
        };
        let new_shown = if new_count == 0 {
            new_at.saturating_sub(1)
        } else {
            new_at
        };
        out.push_str(&format!(
            "@@ -{old_shown},{old_count} +{new_shown},{new_count} @@\n"
        ));
        for line in &script[start..end] {
            let (mark, text) = match line {
                Line::Kept(text) => (' ', *text),
                Line::Removed(text) => ('-', *text),
                Line::Added(text) => ('+', *text),
            };
            out.push(mark);
            out.push_str(text);
            out.push('\n');
        }
    }
    out
}

/// Where each line stands in the old text and in the new one, from 1.
fn places(script: &[Line]) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(script.len());
    let (mut old_at, mut new_at) = (1, 1);
    for line in script {
        out.push((old_at, new_at));
        match line {
            Line::Kept(_) => {
                old_at += 1;
                new_at += 1;
            }
            Line::Removed(_) => old_at += 1,
            Line::Added(_) => new_at += 1,
        }
    }
    out
}

/// The stretches one patch carries: every change, with its own unchanged lines.
fn hunks(script: &[Line]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (at, line) in script.iter().enumerate() {
        if matches!(line, Line::Kept(_)) {
            continue;
        }
        let start = at.saturating_sub(CONTEXT);
        let end = (at + 1 + CONTEXT).min(script.len());
        match out.last_mut() {
            Some((_, last)) if start <= *last => *last = end.max(*last),
            _ => out.push((start, end)),
        }
    }
    out
}
