//! What runs an entry: the program, and what it wants before the file.
//!
//! See `docs/notes/skill-entry.md`.

use std::path::{Path, PathBuf};

use crate::shell_path;

/// The interpreters an entry may name, as each caller resolved them.
///
/// # Invariant
///
/// Looked up by the caller, never here: the server has a configured bash and
/// Python, the command line finds them on PATH, and this crate keeps no opinion
/// about where a program lives. `None` is "not found", and the error naming it is
/// made only when an entry asks for that one.
#[derive(Debug, Clone, Default)]
pub struct Programs {
    pub bash: Option<PathBuf>,
    pub python: Option<PathBuf>,
    pub powershell: Option<PathBuf>,
}

/// The program an entry is run by, and the arguments it wants before the file.
#[derive(Debug, Clone)]
pub struct Runner {
    pub program: PathBuf,
    pub leading: Vec<String>,
    /// A POSIX shell reads `/c/...`, which is not a path Python or Windows can open.
    pub posix: bool,
}

impl Runner {
    /// The entry's path the way this program reads it.
    pub fn spell(&self, entry: &Path) -> String {
        if self.posix {
            shell_path(entry)
        } else {
            entry.display().to_string()
        }
    }

    /// The words this entry is run with: the program's own, the file, then argv.
    pub fn words(&self, entry: &Path, argv: &[String]) -> Vec<String> {
        let mut words = self.leading.clone();
        words.push(self.spell(entry));
        words.extend(argv.iter().cloned());
        words
    }
}

/// An entry is a program of its own, or a script whose first line names one.
pub fn runner(entry: &Path, programs: &Programs) -> Result<Runner, String> {
    let extension = entry
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "sh" | "bash" => shell(programs),
        "py" => Ok(plain(&python(programs)?, Vec::new())),
        "ps1" => Ok(plain(
            &powershell(programs)?,
            ["-NoProfile", "-File"].map(str::to_string).to_vec(),
        )),
        "cmd" | "bat" => Ok(plain(Path::new("cmd"), vec!["/c".to_string()])),
        // No extension worth naming: the file's own `#!` says what runs it.
        _ => match shebang(entry) {
            Some(program) => interpreted(&program, programs),
            None => Ok(plain(entry, Vec::new())),
        },
    }
}

fn plain(program: &Path, leading: Vec<String>) -> Runner {
    Runner {
        program: program.to_path_buf(),
        leading,
        posix: false,
    }
}

fn shell(programs: &Programs) -> Result<Runner, String> {
    let Some(bash) = &programs.bash else {
        return Err(
            "this entry is a shell script and no bash was found: install one, or name it in \
             the config (`GONGGA_BASH`, `[tools.bash] program`)"
                .to_string(),
        );
    };
    Ok(Runner {
        program: bash.clone(),
        leading: Vec::new(),
        posix: true,
    })
}

fn python(programs: &Programs) -> Result<PathBuf, String> {
    programs.python.clone().ok_or_else(|| {
        "this entry is a Python script and no Python was found: install one, or name it in \
         the config (`GONGGA_PYTHON`, `[tools.python] program`)"
            .to_string()
    })
}

fn powershell(programs: &Programs) -> Result<PathBuf, String> {
    programs.powershell.clone().ok_or_else(|| {
        "this entry is a PowerShell script and no PowerShell was found on PATH".to_string()
    })
}

/// The program a `#!` line names, which is the file's answer even under Windows.
fn shebang(entry: &Path) -> Option<String> {
    let first = std::fs::read_to_string(entry).ok()?;
    let line = first.lines().next()?.strip_prefix("#!")?;
    let mut words = line.split_whitespace();
    let mut program = words.next()?;
    let name = file_name(program);
    // `#!/usr/bin/env bash` names the interpreter in its argument, not in `env`.
    if name == "env" {
        program = words.find(|word| !word.starts_with('-'))?;
        return Some(file_name(program));
    }
    Some(name)
}

/// The program a `#!` line names, as this crate can name it.
fn interpreted(program: &str, programs: &Programs) -> Result<Runner, String> {
    if program.starts_with("python") {
        return Ok(plain(&python(programs)?, Vec::new()));
    }
    let shells = ["sh", "bash", "zsh", "dash"];
    if shells.contains(&program) {
        return shell(programs);
    }
    // Named, not resolved: an unknown program is the caller's PATH to answer.
    let named = Path::new(program);
    Ok(Runner {
        program: named.to_path_buf(),
        leading: Vec::new(),
        posix: false,
    })
}

fn file_name(program: &str) -> String {
    program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_string()
}
