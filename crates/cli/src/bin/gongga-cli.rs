//! `gongga-cli`: one server command per process.
//!
//! What a command line *means* is not here. A server command is the server's
//! catalog, and the program that reads it is `python/gongga_cli.py`, installed beside
//! this home's other Python. This file is the name a shell types, plus the two
//! commands the layer answers itself: one starts a process, the other walks the
//! skill roots on this disk, and neither is a server's answer.
//!
//! See `docs/notes/cli-bin-gongga.md`.

use std::ffi::OsString;
use std::path::PathBuf;

/// The commands this program answers itself.
///
/// # Invariant
///
/// Named here and nowhere else: `serve` takes over the process, and `skill` reads
/// `crates/skill`'s roots, which has one implementation and is Rust.
const OWN: [&str; 2] = ["serve", "skill"];

/// The Python command line's name in this home's `lib`.
const SCRIPT: &str = "gongga_cli.py";

fn main() -> anyhow::Result<()> {
    let words: Vec<OsString> = std::env::args_os().skip(1).collect();
    if words
        .first()
        .is_some_and(|word| OWN.iter().any(|own| *own == word.to_string_lossy()))
    {
        let args = link::parse_argv(link::spoken_name(), link::ABOUT);
        args.check_flag_order()?;
        return link::main(args);
    }
    let code = python(&words)?;
    // The command's answer is this process's: a refusal of the server's is a
    // non-zero exit, and a pipeline reads it the same either way.
    std::process::exit(code);
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
