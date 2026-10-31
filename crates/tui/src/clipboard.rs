//! Putting text on the operator's clipboard, with whichever tool this machine has.
//!
//! See `docs/notes/tui-clipboard.md`.

use std::io::Write;
use std::process::{Command, Stdio};

/// Programs that read stdin and put it on the clipboard, tried in order.
///
/// An empty list is a headless box, not a bug.
fn candidates() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(windows) {
        // `clip` ships with Windows; it takes text on stdin and prints nothing.
        &[("clip", &[])]
    } else if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        // Wayland first, then the two X11 tools: whichever is installed.
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

/// Programs that print the clipboard on stdout, tried in order, for a paste the
/// terminal did not deliver itself.
fn readers() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(windows) {
        // `Get-Clipboard` is a cmdlet, so it needs a host to run in.
        &[("powershell", &["-NoProfile", "-Command", "Get-Clipboard"])]
    } else if cfg!(target_os = "macos") {
        &[("pbpaste", &[])]
    } else {
        &[
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-selection", "clipboard", "-o"]),
            ("xsel", &["--clipboard", "--output"]),
        ]
    }
}

/// What a read of the clipboard produced.
pub enum FromClipboard {
    /// The text, as the tool printed it.
    Ok(String),
    /// The clipboard holds nothing a prompt could take.
    Empty,
    /// No clipboard tool could be run at all.
    NoTool,
    /// A tool ran and refused; the message is its own words.
    Failed(String),
}

/// The clipboard's text, for a Ctrl+V the terminal forwarded instead of pasting.
///
/// # Invariant
///
/// A failure is reported, never swallowed, and an empty clipboard is not a
/// failure: nothing to paste has no words to say. See `docs/notes/tui-paste.md`.
pub fn paste() -> FromClipboard {
    paste_with(readers())
}

/// [`paste`] against a named list, so a test exercises the spawn and the pipe
/// without touching the real clipboard.
pub fn paste_with(tools: &[(&'static str, &'static [&'static str])]) -> FromClipboard {
    let mut last: Option<String> = None;
    for (program, args) in tools {
        match Command::new(program).args(*args).output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                return if text.is_empty() {
                    FromClipboard::Empty
                } else {
                    FromClipboard::Ok(text)
                };
            }
            Ok(out) => {
                let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
                last = Some(if said.is_empty() {
                    format!("{program}: exit {:?}", out.status.code())
                } else {
                    format!("{program}: {said}")
                });
            }
            // Not installed: the next candidate is the answer, not an error.
            Err(_) => continue,
        }
    }
    match last {
        Some(message) => FromClipboard::Failed(message),
        None => FromClipboard::NoTool,
    }
}

/// What happened, so the caller can say it in one line.
pub enum Copied {
    /// A tool took the text.
    Ok { chars: usize, tool: &'static str },
    /// Nothing was attempted: there was no text.
    Empty,
    /// No clipboard tool could be run at all.
    NoTool,
    /// A tool ran and refused the text; the message is its own words.
    Failed(String),
}

/// Puts `text` on the system clipboard.
///
/// # Invariant
///
/// A failure is reported, never swallowed: a copy that silently did nothing is
/// worse than one that says it could not.
pub fn copy(text: &str) -> Copied {
    copy_with(text, candidates())
}

/// [`copy`] against a named list, so a test exercises the spawn and the pipe
/// without touching the real clipboard.
pub fn copy_with(text: &str, tools: &[(&'static str, &'static [&'static str])]) -> Copied {
    if text.trim().is_empty() {
        return Copied::Empty;
    }
    let mut last: Option<String> = None;
    for (program, args) in tools {
        let spawned = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            // Not installed: the next candidate is the answer, not an error.
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            if let Err(e) = stdin.write_all(text.as_bytes()) {
                last = Some(format!("{program}: {e}"));
                continue;
            }
        }
        match child.wait_with_output() {
            Ok(out) if out.status.success() => {
                let chars = text.chars().count();
                return Copied::Ok {
                    chars,
                    tool: program,
                };
            }
            Ok(out) => {
                let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
                last = Some(if said.is_empty() {
                    format!("{program}: exit {:?}", out.status.code())
                } else {
                    format!("{program}: {said}")
                });
            }
            Err(e) => last = Some(format!("{program}: {e}")),
        }
    }
    match last {
        Some(message) => Copied::Failed(message),
        None => Copied::NoTool,
    }
}

/// The text a paste should insert: line endings normalized, nothing dropped.
///
/// # Invariant
///
/// A paste arrives as one string, and the input box is multi-line, so a pasted
/// programme must keep its breaks instead of submitting at the first one.
pub fn paste_text(raw: &str) -> String {
    // Windows clipboards hand back CRLF; `\r` would otherwise render as a glyph.
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pasted block keeps its line structure; only the endings are normalized.
    #[test]
    fn a_paste_keeps_its_lines() {
        assert_eq!(paste_text("a\r\nb"), "a\nb");
        assert_eq!(paste_text("a\rb"), "a\nb");
        assert_eq!(paste_text("a\nb\n"), "a\nb\n");
        // Tabs and trailing spaces are the operator's text, not ours to fix.
        assert_eq!(paste_text("\ta \n"), "\ta \n");
    }

    /// Empty text is reported as such, never handed to a clipboard tool.
    #[test]
    fn nothing_is_not_a_copy() {
        assert!(matches!(copy(""), Copied::Empty));
        assert!(matches!(copy("  \n "), Copied::Empty));
    }

    /// Every platform names at least one tool, so a copy is always attempted.
    #[test]
    fn a_tool_is_always_named() {
        assert!(!candidates().is_empty());
        assert!(candidates().iter().all(|(p, _)| !p.is_empty()));
        assert!(!readers().is_empty());
        assert!(readers().iter().all(|(p, _)| !p.is_empty()));
    }

    /// A read hands back exactly what the tool printed, and an empty clip is a
    /// verdict of its own rather than a failure.
    #[test]
    fn a_read_reports_what_the_tool_printed() {
        let printing: &[(&str, &[&str])] = if cfg!(windows) {
            &[("cmd", &["/c", "echo hello"])]
        } else {
            &[("echo", &["hello"])]
        };
        match paste_with(printing) {
            FromClipboard::Ok(text) => assert_eq!(text.trim_end(), "hello"),
            other => panic!(
                "a printing tool must read as text: {}",
                matches!(other, FromClipboard::Empty)
            ),
        }
        let silent: &[(&str, &[&str])] = if cfg!(windows) {
            &[("cmd", &["/c", "exit 0"])]
        } else {
            &[("true", &[])]
        };
        assert!(matches!(paste_with(silent), FromClipboard::Empty));
        let none: &[(&str, &[&str])] = &[("a-program-that-does-not-exist-anywhere", &[])];
        assert!(matches!(paste_with(none), FromClipboard::NoTool));
    }

    /// A missing tool is skipped, not fatal: the next candidate still gets a turn.
    #[test]
    fn an_absent_tool_is_skipped() {
        let none: &[(&str, &[&str])] = &[("a-program-that-does-not-exist-anywhere", &[])];
        assert!(matches!(copy_with("x", none), Copied::NoTool));
    }

    /// A tool that runs and fails is reported with its own words, never as success.
    #[test]
    fn a_failing_tool_is_reported() {
        // `cmd /c exit 3` runs everywhere on Windows; elsewhere `false` is the same idea.
        let failing: &[(&str, &[&str])] = if cfg!(windows) {
            &[("cmd", &["/c", "exit 3"])]
        } else {
            &[("false", &[])]
        };
        match copy_with("x", failing) {
            Copied::Failed(message) => assert!(!message.is_empty(), "a failure must say why"),
            other => panic!(
                "a failing tool must not read as success: {:?}",
                matches!(other, Copied::Ok { .. })
            ),
        }
    }

    /// The happy path: the text reaches a real process and its exit status decides.
    #[test]
    fn a_tool_that_accepts_the_text_reports_success() {
        // Both read stdin to EOF and exit 0; their output already goes to null.
        let ok: &[(&str, &[&str])] = if cfg!(windows) {
            &[("more", &[])]
        } else {
            &[("cat", &[])]
        };
        match copy_with("hello", ok) {
            Copied::Ok { chars, tool } => {
                assert_eq!(chars, 5);
                assert!(!tool.is_empty());
            }
            Copied::NoTool => eprintln!("skipping: no `{}` on this machine", ok[0].0),
            Copied::Failed(why) => panic!("a working tool reported failure: {why}"),
            Copied::Empty => panic!("text was not empty"),
        }
    }
}
