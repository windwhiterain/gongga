//! Putting text on the operator's clipboard, with whichever tool this machine has.
//!
//! See `docs/notes/tui-clipboard.md`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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

/// Programs that read an image from the clipboard, tried in order.
///
/// See `docs/notes/tui-clipboard.md`.
fn image_candidates() -> Vec<ImageTool> {
    if cfg!(windows) {
        // `-STA` or `GetImage()` throws; `-File` or no `$args[0]`.
        let args = powershell_image_args();
        vec![ImageTool::writes_to_path("powershell", args)]
    } else if cfg!(target_os = "macos") {
        // The destination path is appended as the last argument and addressed as
        // `item 1 of my arguments` in AppleScript.
        vec![ImageTool::writes_to_path(
            "osascript",
            &[
                "-e",
                "set f to open for access POSIX file (item 1 of my arguments) with write permission\nwrite (the clipboard as «class PNGf») to f\nclose access f",
            ],
        )]
    } else {
        // Linux tools print on stdout; the shell redirects it into `$1`.
        vec![
            ImageTool::writes_to_path("sh", &["-c", "wl-paste -t image/png > \"$1\"", "_"]),
            ImageTool::writes_to_path(
                "sh",
                &[
                    "-c",
                    "xclip -selection clipboard -t image/png -o > \"$1\"",
                    "_",
                ],
            ),
        ]
    }
}

/// Returns the path to the cached PowerShell image-paste script, writing it
/// first if it does not yet exist.
///
/// # Invariant
///
/// The script lives under the temp directory so it is recreated automatically
/// when the temp directory is cleared, and it is written atomically so a
/// concurrent paste never reads a half-written file.
#[cfg(windows)]
fn powershell_image_script() -> &'static str {
    use std::sync::OnceLock;
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join("gongga-tui");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("gongga-clip-image.ps1");
        let script = "Add-Type -AssemblyName System.Windows.Forms;\n$img = [System.Windows.Forms.Clipboard]::GetImage();\nif ($img) {\n    $path = $args[0];\n    $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png);\n}\n";
        let tmp = dir.join("gongga-clip-image.ps1.tmp");
        if let Ok(mut f) = fs::File::create(&tmp) {
            let _ = f.write_all(script.as_bytes());
            let _ = f.sync_all();
            let _ = fs::rename(&tmp, &path);
        }
        path.to_string_lossy().into_owned()
    })
    .as_str()
}

/// The argument list for the PowerShell image reader, built once.
///
/// # Invariant
///
/// The list is built once and kept for the life of the process: the script path
/// is stable, and building it per paste would leak the slice every time.
#[cfg(windows)]
fn powershell_image_args() -> &'static [&'static str] {
    use std::sync::OnceLock;
    static ARGS: OnceLock<&'static [&'static str]> = OnceLock::new();
    ARGS.get_or_init(|| {
        let script = powershell_image_script();
        let args: Vec<&'static str> = vec!["-NoProfile", "-STA", "-File", script];
        Box::leak(args.into_boxed_slice())
    })
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

/// The clipboard's text, for a paste key the terminal forwarded instead of pasting.
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
        let status = match child.wait() {
            Ok(status) => status,
            Err(e) => {
                last = Some(format!("{program}: {e}"));
                continue;
            }
        };
        if status.success() {
            return Copied::Ok {
                chars: text.chars().count(),
                tool: program,
            };
        }
        let said = match child.stderr {
            Some(mut err) => {
                let mut buf = String::new();
                let _ = std::io::Read::read_to_string(&mut err, &mut buf);
                buf.trim().to_string()
            }
            None => String::new(),
        };
        last = Some(if said.is_empty() {
            format!("{program}: exit {:?}", status.code())
        } else {
            format!("{program}: {said}")
        });
    }
    match last {
        Some(message) => Copied::Failed(message),
        None => Copied::NoTool,
    }
}

/// What a read of the clipboard's image produced.
///
/// # Invariant
///
/// A saved file outlives the paste because the message references it at send
/// time; a missing image is not a failure and no refusal is swallowed.
#[derive(Debug)]
pub enum ImageClipboard {
    /// The image was written to a file; this is its path.
    Saved(PathBuf),
    /// The clipboard holds no image (it may hold text — the caller falls back).
    NoImage,
    /// No clipboard tool could be run at all.
    NoTool,
    /// A tool ran and refused; the message is its own words.
    Failed(String),
}

/// A clipboard image tool: either it writes the file itself, or it prints the
/// bytes on stdout for the caller to capture.
#[derive(Clone, Copy, Debug)]
pub struct ImageTool {
    program: &'static str,
    args: &'static [&'static str],
    mode: ImageToolMode,
}

#[derive(Clone, Copy, Debug)]
enum ImageToolMode {
    /// The destination path is appended as the last argument; the tool writes
    /// the image file itself.
    WritesToPath,
    /// The tool prints the image bytes on stdout; the caller redirects them to
    /// the destination file.
    PrintsToStdout,
}

impl ImageTool {
    const fn writes_to_path(program: &'static str, args: &'static [&'static str]) -> Self {
        Self {
            program,
            args,
            mode: ImageToolMode::WritesToPath,
        }
    }

    #[allow(dead_code)]
    const fn prints_to_stdout(program: &'static str, args: &'static [&'static str]) -> Self {
        Self {
            program,
            args,
            mode: ImageToolMode::PrintsToStdout,
        }
    }
}

/// The directory where pasted images are saved.
///
/// # Invariant
///
/// The directory is created if it does not exist, so the tools that follow
/// never fail because a temp path was missing.
fn dest_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("gongga-tui");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// A fresh file name for a pasted image.
fn fresh_image_path(dest_dir: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    dest_dir.join(format!("gongga-paste-{pid}-{n}.png"))
}

/// The clipboard's image, if it holds one.
///
/// # Invariant
///
/// A failure is reported, never swallowed; an empty or text-only clipboard is
/// `NoImage`, not a failure.
pub fn paste_image() -> ImageClipboard {
    paste_image_with(&image_candidates(), &dest_dir())
}

/// [`paste_image`] against a named list of tools and a destination.
///
/// The testable seam: a stub tool touches nothing real.
pub fn paste_image_with(tools: &[ImageTool], dest_dir: &Path) -> ImageClipboard {
    let _ = fs::create_dir_all(dest_dir);
    let mut last: Option<String> = None;
    for tool in tools {
        let dest = fresh_image_path(dest_dir);
        match run_image_tool(tool, &dest) {
            Some(Ok(true)) => return ImageClipboard::Saved(dest),
            Some(Ok(false)) => {
                let _ = fs::remove_file(&dest);
                return ImageClipboard::NoImage;
            }
            Some(Err(message)) => last = Some(message),
            None => continue,
        }
    }
    match last {
        Some(message) => ImageClipboard::Failed(message),
        None => ImageClipboard::NoTool,
    }
}

/// Runs one image candidate.
///
/// `None` when it could not be spawned, so the next candidate gets a turn.
fn run_image_tool(tool: &ImageTool, dest: &Path) -> Option<Result<bool, String>> {
    match tool.mode {
        ImageToolMode::WritesToPath => {
            let mut cmd = Command::new(tool.program);
            cmd.args(tool.args).arg(dest);
            match cmd.output() {
                Ok(out) if out.status.success() => {
                    Some(Ok(fs::metadata(dest).map(|m| m.len() > 0).unwrap_or(false)))
                }
                Ok(out) => {
                    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    Some(Err(if said.is_empty() {
                        format!("{}: exit {:?}", tool.program, out.status.code())
                    } else {
                        format!("{}: {}", tool.program, said)
                    }))
                }
                // Not installed: let the caller try the next candidate.
                Err(_) => None,
            }
        }
        ImageToolMode::PrintsToStdout => {
            let mut cmd = Command::new(tool.program);
            cmd.args(tool.args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = match cmd.spawn() {
                Ok(child) => child,
                // Not installed: let the caller try the next candidate.
                Err(_) => return None,
            };
            let stdout = child.stdout.take()?;
            let mut file = match fs::File::create(dest) {
                Ok(file) => file,
                Err(e) => {
                    return Some(Err(format!(
                        "{}: could not create {dest:?}: {e}",
                        tool.program
                    )));
                }
            };
            let mut reader = std::io::BufReader::new(stdout);
            let copied = match std::io::copy(&mut reader, &mut file) {
                Ok(n) => n,
                Err(e) => {
                    return Some(Err(format!(
                        "{}: failed to write {dest:?}: {e}",
                        tool.program
                    )));
                }
            };
            let status = match child.wait() {
                Ok(status) => status,
                Err(e) => return Some(Err(format!("{}: {e}", tool.program))),
            };
            if status.success() {
                Some(Ok(copied > 0))
            } else {
                let said = match child.stderr {
                    Some(mut err) => {
                        let mut buf = String::new();
                        let _ = std::io::Read::read_to_string(&mut err, &mut buf);
                        buf.trim().to_string()
                    }
                    None => String::new(),
                };
                Some(Err(if said.is_empty() {
                    format!("{}: exit {:?}", tool.program, status.code())
                } else {
                    format!("{}: {}", tool.program, said)
                }))
            }
        }
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
