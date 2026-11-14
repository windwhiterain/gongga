//! `skill`: run what a skill publishes -- an entry, or a package's function.
//!
//! See `docs/notes/client-skill-command.md`.

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
/// publishes and the rest are one JSON word, `{"function": "<name>", "args": {...}}`,
/// sent as the line its stdin is. A skill whose own name is `ls` or `run` is still
/// reachable as `skill run <name>`.
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

/// Runs a skill with this process's stdio, and leaves as it did.
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
    // The home's Python surface, so a package imports the contract and the client.
    let lib = home.join("lib");
    let launch = skill::runner_of(
        skill.dir(),
        skill.entry.as_deref(),
        &programs(),
        lib.is_dir().then_some(lib.as_path()),
    )
    .map_err(|why| anyhow!(why))?;
    let target = skill.entry.as_deref().unwrap_or(skill.dir());
    let (program, words, request) = match &launch {
        skill::Launch::Entry(runner) => (runner.program.clone(), runner.words(target, argv), None),
        skill::Launch::Package(runner) => {
            let (function, args) = package_call(argv)?;
            let request = skill::request(skill.dir(), &function, &args);
            (
                runner.program.clone(),
                runner.leading.clone(),
                Some(request),
            )
        }
    };
    let mut command = Command::new(&program);
    command
        .args(&words)
        .stdin(match request {
            Some(_) => Stdio::piped(),
            None => Stdio::inherit(),
        })
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if lib.is_dir() {
        command.env("GONGGA_SKILL_LIB", &lib);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("running {}", target.display()))?;
    // The request, then the end of it: the dropped pipe is the contract's EOF.
    if let (Some(text), Some(mut pipe)) = (request, child.stdin.take()) {
        pipe.write_all(text.as_bytes())
            .with_context(|| format!("writing the request to {}", program.display()))?;
    }
    let status = child
        .wait()
        .with_context(|| format!("waiting for {}", program.display()))?;
    let _ = std::io::stdout().flush();
    // The entry answered, not this command. A signal has no code.
    std::process::exit(status.code().unwrap_or(1));
}

/// The one word a package's call is: `{"function": "<name>", "args": {...}}`.
///
/// # Invariant
///
/// One JSON object and no flags: a package's call is the contract's request without the
/// skill's path, so `skill <name> '<json>'` reaches the runner as the line it reads.
/// Nothing here parses a command line back out of words -- that parser was removed on
/// purpose, and a second one would answer the same question twice.
fn package_call(words: &[String]) -> anyhow::Result<(String, serde_json::Value)> {
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Ok((String::new(), serde_json::Value::Object(Default::default())));
    }
    let call: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        anyhow!(
            "a package's call is one JSON object, `{{\"function\": \"<name>\", \"args\": {{}}}}`, \
             and this does not parse: {e}"
        )
    })?;
    let serde_json::Value::Object(table) = call else {
        return Err(anyhow!(
            "a package's call is one JSON object of `function` and `args`, and this is {}",
            kind(&call)
        ));
    };
    let function = match table.get("function") {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(name)) => name.trim().to_string(),
        Some(other) => {
            return Err(anyhow!(
                "`function` is the name a package publishes, and this is {}",
                kind(other)
            ));
        }
    };
    let args = match table.get("args") {
        None | Some(serde_json::Value::Null) => serde_json::Value::Object(Default::default()),
        Some(serde_json::Value::Object(table)) => serde_json::Value::Object(table.clone()),
        Some(other) => {
            return Err(anyhow!(
                "`args` is the function's keyword arguments, an object of named ones, and this is {}",
                kind(other)
            ));
        }
    };
    Ok((function, args))
}

/// What a JSON value is called in a sentence about the call.
fn kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
    }
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
pub fn python() -> anyhow::Result<PathBuf> {
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
