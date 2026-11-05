//! `skill`: run the entry a skill declares, or say which file a name opens.
//!
//! See `docs/notes/cli-skill-command.md`.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow};

/// `skill <name> [args…]`, run here: a skill's entry is a file, not a server command.
///
/// # Invariant
///
/// Everything after the name is the entry's argv, unparsed and unquoted, and the
/// entry's exit code is this process's. A skill whose own name is `ls`, `which` or
/// `run` is still reachable as `skill run <name>`.
pub fn dispatch(home: &Path, tokens: &[String]) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("reading this process's directory")?;
    let roots = crate::commands::roots(home, Some(&cwd));
    match tokens.first().map(String::as_str) {
        None => Err(anyhow!(
            "skill: name one (`skill <name> [args…]`), or `skill ls` for what is installed"
        )),
        Some("ls") => list(&roots),
        Some("which") => which(&roots, tokens.get(1).map(String::as_str)),
        Some("run") => launch(
            &roots,
            tokens.get(1).map(String::as_str),
            tokens.get(2..).unwrap_or_default(),
        ),
        Some(name) => launch(&roots, Some(name), tokens.get(1..).unwrap_or_default()),
    }
}

/// One line per skill, most specific first, and a line for every name a root hides.
fn list(roots: &skill::Roots) -> anyhow::Result<()> {
    let all = roots.all();
    if all.is_empty() {
        println!(
            "no skill is installed: nothing under {} either, and no checkout's `.agents/skills`",
            roots.home.display()
        );
        return Ok(());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for found in &all {
        if !seen.insert(found.name.as_str()) {
            shadowed(found);
            continue;
        }
        println!(
            "{}\t{}\t{}\t{}",
            found.name,
            found.bucket.as_str(),
            entry_name(found),
            found.dir().display()
        );
    }
    Ok(())
}

/// Where a name resolves, and what else holds that name.
fn which(roots: &skill::Roots, name: Option<&str>) -> anyhow::Result<()> {
    let name = name.ok_or_else(|| anyhow!("which: name a skill, e.g. `skill which image`"))?;
    let found = roots.named(name);
    let Some(first) = found.first() else {
        return Err(anyhow!(
            "no skill named {name:?} is installed; `skill ls` lists what is"
        ));
    };
    println!(
        "{}\t{}\t{}\t{}",
        first.name,
        first.bucket.as_str(),
        first.path.display(),
        first
            .entry
            .as_ref()
            .map_or_else(|| "-".to_string(), |entry| entry.display().to_string())
    );
    for other in &found[1..] {
        shadowed(other);
    }
    Ok(())
}

/// Runs a skill's entry with this process's stdio, and leaves as it did.
fn launch(roots: &skill::Roots, name: Option<&str>, argv: &[String]) -> anyhow::Result<()> {
    let name = name.ok_or_else(|| anyhow!("run: name a skill, e.g. `skill run ls`"))?;
    let found = roots.named(name);
    let Some(skill) = found.first() else {
        return Err(anyhow!(
            "no skill named {name:?} is installed; `skill ls` lists what is"
        ));
    };
    for other in &found[1..] {
        shadowed(other);
    }
    let Some(entry) = &skill.entry else {
        return Err(anyhow!(
            "{name} is a body and nothing to run: read {} and do what it says",
            skill.path.display()
        ));
    };
    let run = runner(entry)?;
    let mut command = Command::new(&run.program);
    command
        .args(&run.leading)
        .arg(run.spell(entry))
        .args(argv)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command
        .status()
        .with_context(|| format!("running {}", entry.display()))?;
    let _ = std::io::stdout().flush();
    // The entry answered, not this command. A signal has no code.
    std::process::exit(status.code().unwrap_or(1));
}

/// A line for a skill installed under a name that a more specific root already took.
fn shadowed(found: &skill::Skill) {
    eprintln!(
        "shadowed\t{}\t{}\t{}",
        found.name,
        found.bucket.as_str(),
        found.path.display()
    );
}

fn entry_name(found: &skill::Skill) -> String {
    found.entry.as_ref().map_or_else(
        || "-".to_string(),
        |entry| {
            entry
                .file_name()
                .map(|file| file.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
    )
}

/// The program an entry is run by, and the arguments it wants before the file.
struct Runner {
    program: PathBuf,
    leading: Vec<String>,
    /// A POSIX shell reads `/c/...`, which is not a path Python or Windows can open.
    posix: bool,
}

impl Runner {
    /// The entry's path the way this program reads it.
    fn spell(&self, entry: &Path) -> String {
        if self.posix {
            skill::shell_path(entry)
        } else {
            entry.display().to_string()
        }
    }
}

/// An entry is a program of its own, or a script whose first line names one.
fn runner(entry: &Path) -> anyhow::Result<Runner> {
    let extension = entry
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let plain = |program: PathBuf, leading: Vec<String>| Runner {
        program,
        leading,
        posix: false,
    };
    Ok(match extension.as_str() {
        "sh" | "bash" => Runner {
            program: which::which("bash").context("no bash on PATH")?,
            leading: Vec::new(),
            posix: true,
        },
        "py" => plain(python()?, Vec::new()),
        "ps1" => plain(
            powershell()?,
            ["-NoProfile", "-File"].map(str::to_string).to_vec(),
        ),
        "cmd" | "bat" => plain(PathBuf::from("cmd"), vec!["/c".to_string()]),
        // No extension worth naming: the file's own `#!` says what runs it.
        _ => match shebang(entry) {
            Some(program) => interpreted(&program)?,
            None => plain(entry.to_path_buf(), Vec::new()),
        },
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

fn file_name(program: &str) -> String {
    program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_string()
}

/// The program a `#!` line names.
fn interpreted(program: &str) -> anyhow::Result<Runner> {
    let program = if program.starts_with("python") {
        python()?
    } else {
        which::which(program).with_context(|| format!("no {program} on PATH"))?
    };
    let posix = matches!(
        program_name(&program).as_str(),
        "sh" | "bash" | "zsh" | "dash"
    );
    Ok(Runner {
        program,
        leading: Vec::new(),
        posix,
    })
}

/// The last component of a program's path, which is how a shell is recognised.
fn program_name(program: &Path) -> String {
    program
        .file_stem()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// The first Python that runs: Windows' Store alias sits on `PATH` and answers 49.
fn python() -> anyhow::Result<PathBuf> {
    for candidate in ["python3", "python", "py"] {
        let Ok(program) = which::which(candidate) else {
            continue;
        };
        let mut probe = Command::new(&program);
        probe
            .args(["-c", "import sys"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Python is a console program, and the probe has no console to hand down.
        console::apply(&mut probe, 0);
        let runs = probe.status().is_ok_and(|status| status.success());
        if runs {
            return Ok(program);
        }
    }
    Err(anyhow!(
        "no Python runs on this machine (tried python3, python, py)"
    ))
}

fn powershell() -> anyhow::Result<PathBuf> {
    which::which("pwsh")
        .or_else(|_| which::which("powershell"))
        .context("no PowerShell on PATH")
}
