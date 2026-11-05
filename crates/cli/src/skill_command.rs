//! `skill`: run what a skill publishes -- an entry, or a package's function.
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
/// entry's exit code is this process's. A package's first word names the function it
/// publishes, and the rest bind to that function's parameters. A skill whose own name
/// is `ls` or `run` is still reachable as `skill run <name>`.
pub fn dispatch(home: &Path, tokens: &[String]) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("reading this process's directory")?;
    let roots = crate::commands::roots(home, Some(&cwd));
    match tokens.first().map(String::as_str) {
        None => Err(anyhow!(
            "skill: name one (`skill <name> [args…]`), or `skill ls` for what is installed"
        )),
        Some("ls") => list(&roots),
        Some("run") => launch(
            &roots,
            home,
            tokens.get(1).map(String::as_str),
            tokens.get(2..).unwrap_or_default(),
        ),
        Some(name) => launch(
            &roots,
            home,
            Some(name),
            tokens.get(1..).unwrap_or_default(),
        ),
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
        // The server's four columns, so a reader and a model see one shape.
        println!(
            "{}\t{}\t{}\t{}",
            found.name,
            found.bucket.as_str(),
            found.description.replace(['\t', '\n'], " "),
            found.path.display()
        );
    }
    Ok(())
}

/// Runs a skill's entry with this process's stdio, and leaves as it did.
fn launch(
    roots: &skill::Roots,
    home: &Path,
    name: Option<&str>,
    argv: &[String],
) -> anyhow::Result<()> {
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
    // A package needs no declared entry; without one or the other, it is a body.
    if skill.entry.is_none() && !skill.dir().join("__init__.py").is_file() {
        return Err(anyhow!(
            "{name} is a body and nothing to run: read {} and do what it says",
            skill.path.display()
        ));
    }
    let run = skill::runner_of(skill.dir(), skill.entry.as_deref(), &programs())
        .map_err(|why| anyhow!(why))?;
    let target = skill.entry.as_deref().unwrap_or(skill.dir());
    let mut command = Command::new(&run.program);
    command
        .args(run.words(target, argv))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The home's Python surface, so a package imports the contract and the client.
    let lib = home.join("lib");
    if lib.is_dir() {
        command.env("GONGGA_SKILL_LIB", &lib);
    }
    let status = command
        .status()
        .with_context(|| format!("running {}", target.display()))?;
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

/// What this machine can run an entry with: found here, chosen by the crate's rule.
fn programs() -> skill::Programs {
    skill::Programs {
        bash: which::which("bash").ok(),
        python: python().ok(),
        powershell: powershell().ok(),
    }
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
