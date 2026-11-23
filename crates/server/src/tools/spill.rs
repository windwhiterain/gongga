//! Capping what a tool answers with, and spilling the rest to a file.
//!
//! See `docs/notes/server-tools-bash.md`.

use std::path::Path;

/// Caps tool output: the head and the tail survive, the rest is spilled to disk.
///
/// # Invariant
///
/// Characters, not bytes: a cap is a length somebody stated, and the notice says
/// what it took away in the same unit. Counting bytes makes a call's number mean
/// something else in every language but English.
///
/// # Invariant
///
/// `None` as the directory names no path: a session on a peer cannot promise a file
/// on a machine its reader is not sitting at. See `docs/notes/server-remote.md`.
pub fn cap_output(
    prompts: &crate::prompts::Prompts,
    text: &str,
    max: usize,
    dir: Option<&Path>,
    label: &str,
) -> String {
    let total_chars = text.chars().count();
    if max == 0 || total_chars <= max {
        return text.to_string();
    }
    // The split is in characters, so a cut never lands inside one.
    let head_chars = max * 3 / 5;
    let tail_chars = max - head_chars;
    let head = byte_of_char(text, head_chars);
    let tail = byte_of_char(text, total_chars - tail_chars);
    let omitted = total_chars - head_chars - tail_chars;
    let spilled = dir.and_then(|dir| spill(dir, label, text));
    let omitted = omitted.to_string();
    let total = total_chars.to_string();
    let notice = match &spilled {
        Some(path) => prompts.render(
            "output_cap",
            &[
                ("omitted", omitted.as_str()),
                ("total", total.as_str()),
                ("path", path.as_str()),
            ],
        ),
        None => prompts.render("output_omitted", &[("omitted", omitted.as_str())]),
    };
    format!("{}{}{}", &text[..head], notice, &text[tail..])
}
/// The byte index of the `chars`-th character, clamping to the end.
fn byte_of_char(s: &str, chars: usize) -> usize {
    s.char_indices()
        .nth(chars)
        .map(|(at, _)| at)
        .unwrap_or(s.len())
}
/// Writes the full output under `dir`, returning the path in Git Bash dialect.
pub(crate) fn spill(dir: &Path, label: &str, text: &str) -> Option<String> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let safe = slug(label);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{safe}-{stamp}.out"));
    if std::fs::write(&path, text).is_err() {
        return None;
    }
    Some(skill::shell_path(&path))
}
/// Turns a label into one path component, keeping Unicode alphanumerics.
///
/// See `docs/notes/server-tools-bash.md`.
fn slug(label: &str) -> String {
    const MAX_CHARS: usize = 48;
    let mut out = String::new();
    let mut chars = 0;
    let mut last_was_dash = false;
    for c in label.chars() {
        // Unicode-aware on purpose: it keeps CJK and accents and still drops `/`, `:` and spaces.
        let kept = if c.is_alphanumeric() || c == '_' {
            c
        } else {
            '-'
        };
        if kept == '-' {
            // Collapse runs, and never start a name with one.
            if last_was_dash || out.is_empty() {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        if chars >= MAX_CHARS {
            break;
        }
        chars += 1;
        out.push(kept);
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "output".to_string()
    } else {
        out
    }
}
