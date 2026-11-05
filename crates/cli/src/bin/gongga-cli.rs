//! `gongga-cli`: the name a shell types; the rest is `python/gongga_cli.py`.

use std::ffi::OsString;
use std::path::PathBuf;

/// What this program answers itself: `serve` and the local `skill`.
const OWN: [&str; 2] = ["serve", "skill"];

/// The Python command line's name in this home's `lib`.
const SCRIPT: &str = "gongga_cli.py";

fn main() -> anyhow::Result<()> {
    let words: Vec<OsString> = std::env::args_os().skip(1).collect();
    if command(&words).is_some_and(|name| OWN.contains(&name.as_str())) {
        let args = link::parse_argv(link::spoken_name(), link::ABOUT);
        args.check_flag_order()?;
        return link::main(args);
    }
    let code = python(&words)?;
    // The command's answer is this process's exit status.
    std::process::exit(code);
}

/// The command a line names: the first word that is not a flag or a flag's value.
///
/// # Invariant
///
/// `--home /c/x` and `--server 127.0.0.1:9700` come before the command, so "the
/// first word" is not the command. Which flags take a value is the layer's list, and
/// asking it here would be a second copy of one answer -- a word that starts with a
/// dash is skipped, and so is the word after it when the flag carries no `=`.
fn command(words: &[OsString]) -> Option<String> {
    let mut i = 0;
    while i < words.len() {
        let word = words[i].to_string_lossy().into_owned();
        if word.starts_with("--") {
            i += if word.contains('=') { 1 } else { 2 };
            continue;
        }
        return Some(word);
    }
    None
}

/// Runs the Python command line with these words, and answers its exit code.
///
/// # Invariant
///
/// The words are passed through unparsed and unquoted: whether a bare word is a
/// session or a message is the published catalog's business, and this file knows no
/// command. A home with no `lib/gongga_cli.py` never started a server, which is said
/// as what it is rather than as a missing file.
fn python(words: &[OsString]) -> anyhow::Result<i32> {
    let home = home(words);
    let script = home.join("lib").join(SCRIPT);
    if !script.is_file() {
        anyhow::bail!(
            "no command line to run: {} is not there. It is installed when a server \
             starts on this home, so start one first",
            script.display()
        );
    }
    let program = interpreter()?;
    let mut command = std::process::Command::new(&program);
    command.arg(&script).args(words);
    console::apply(&mut command, 0);
    let status = command
        .status()
        .map_err(|e| anyhow::anyhow!("running {}: {e}", program.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// This home: named for this process, else stated in the words, else the default.
///
/// # Invariant
///
/// `--home` is read here, not parsed: the words are the command's, and a command
/// that happens to want a `--home` of its own gets the same one this file used.
fn home(words: &[OsString]) -> PathBuf {
    if let Some(home) = std::env::var_os("GONGGA_HOME") {
        return PathBuf::from(home);
    }
    let mut i = 0;
    while i < words.len() {
        let word = words[i].to_string_lossy().into_owned();
        if let Some(rest) = word.strip_prefix("--home=") {
            return PathBuf::from(rest);
        }
        if word == "--home" {
            if let Some(next) = words.get(i + 1) {
                return PathBuf::from(next);
            }
        }
        i += 1;
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gongga")
}

/// The first Python that runs, in the crate that already answers that question.
fn interpreter() -> anyhow::Result<PathBuf> {
    link::python_program()
}
