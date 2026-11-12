//! Running a skill: the entry it declares, with its words and no shell.
//!
//! See `docs/notes/server-tools-skill.md`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::words;
use crate::tools::{ToolContext, ToolOutcome};
use skill::Skill;

/// How one skill is started: the programs, the home's import, and the run's bounds.
pub struct Skills {
    /// What an entry may be started by, as the server resolved them.
    pub programs: ::skill::Programs,
    /// Where this home keeps `gongga_skill.py` and `gongga.py`, which a package imports.
    pub lib: Option<PathBuf>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Skills {
    /// Runs one installed skill as `ctx`'s session, with `argv` as its words.
    ///
    /// # Invariant
    ///
    /// One door for every form, and `run_skill` is the only caller: a package binds the
    /// words to a published function, an entry starts the file its frontmatter names,
    /// and a body is refused, naming the `SKILL.md` to read. No shell stands between
    /// the words and the program, so a word arrives as it was written.
    pub async fn run(&self, skill: &Skill, argv: Vec<String>, ctx: &ToolContext) -> ToolOutcome {
        // A package skill needs no declared entry: its callable is discovered.
        if skill.entry.is_none() && !skill.dir().join("__init__.py").is_file() {
            return ToolOutcome::err(format!(
                "{} is a body and nothing to run: read {} and do what it says",
                skill.name,
                skill.path.display()
            ));
        }
        let run = match ::skill::runner_of(skill.dir(), skill.entry.as_deref(), &self.programs) {
            Ok(run) => run,
            Err(why) => return ToolOutcome::err(why),
        };
        let target = skill.entry.as_deref().unwrap_or(skill.dir());
        // The entry is this machine's file whatever machine the session works on.
        let at = ctx.dir(Machine::Local);
        // The package form's second half: a skill's Python imports the contract.
        let mut env: Vec<(String, String)> = ctx.env.clone();
        if let Some(lib) = self.lib.as_deref() {
            env.push((
                "GONGGA_SKILL_LIB".to_string(),
                lib.to_string_lossy().into_owned(),
            ));
        }
        let outcome = match words::run(
            &run.program,
            &run.words(target, &argv),
            &at,
            &env,
            self.timeout,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(why) => return ToolOutcome::err(why),
        };
        let mut text = outcome.stdout;
        if !outcome.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&outcome.stderr);
        }
        let text = ctx.cap(
            Machine::Local,
            "run_skill",
            None,
            text,
            self.max_output_chars,
        );
        let code = outcome.code;
        // The entry's own exit code is this call's: a non-zero one is the skill
        // failing, not the command that asked for it.
        let report = if text.is_empty() {
            format!("[exit status {code}]")
        } else {
            format!(
                "{text}
[exit status {code}]"
            )
        };
        ToolOutcome {
            output: report,
            ok: code == 0,
        }
    }
}

/// The words one `run_skill` call gives its skill, read off SPEC's `Cmd`.
///
/// # Invariant
///
/// `cmd` is `{name, args}` and never a command line: the name first, then its arguments,
/// spelled as an object of named ones (`{"tier": "flash"}` reaches the skill as `--tier
/// flash`) or as a list passed through as written. `--key value` is what `gongga_skill.py`
/// binds to a parameter by name, so both spellings end as the same argv.
pub fn call_words(cmd: Option<&str>) -> Result<Vec<String>, String> {
    let raw = cmd.map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        return Err(
            "run_skill needs a `cmd`: the words the skill runs with, \n             `{\"name\": \"<function>\", \"args\": {}}` or `{\"args\": [\"<word>\"]}`"
                .to_string(),
        );
    }
    // A bare word is the whole call: the function, or the entry's one argument.
    if !raw.starts_with('{') {
        return Ok(vec![raw.to_string()]);
    }
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| format!("`cmd` is an object only when it is a whole call: {e}"))?;
    let Some(call) = value.as_object() else {
        return Err(
            "`cmd` is a call, so it is an object: {\"name\": \"<function>\", \"args\": {}}"
                .to_string(),
        );
    };
    let mut words: Vec<String> = Vec::new();
    if let Some(name) = call
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        words.push(name.to_string());
    }
    match call.get("args") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                match item.as_str() {
                    Some(word) => words.push(word.to_string()),
                    None => {
                        return Err(format!(
                            "`args` is a list of words, and one of them is {}",
                            crate::tools::json_type(item)
                        ));
                    }
                }
            }
        }
        Some(Value::Object(table)) => {
            for (key, value) in table {
                words.push(format!("--{key}"));
                words.push(match value {
                    Value::String(word) => word.clone(),
                    other => other.to_string(),
                });
            }
        }
        Some(other) => {
            return Err(format!(
                "`args` is an object of named arguments or a list of words, and this one is {}",
                crate::tools::json_type(other)
            ));
        }
    }
    Ok(words)
}

/// What this server can start an entry with, from the configuration it resolved.
pub fn programs(cfg: &crate::config::Config) -> ::skill::Programs {
    ::skill::Programs {
        bash: cfg.bash.program.clone(),
        python: cfg
            .python
            .program
            .clone()
            .or_else(|| crate::config::detect_python()),
        powershell: which::which("pwsh")
            .or_else(|_| which::which("powershell"))
            .ok(),
    }
}

/// Every skill a session standing on `machine` in `cwd` can open.
///
/// # Invariant
///
/// One walk, shared with the command layer and with `@skill(...)`: the project layer
/// is the checkout's, so a session whose directory is on a remote is handed the home's
/// and the machine's rows. See `docs/notes/server-tools-skill.md`.
pub fn installed_for(cwd: &Path, machine: Machine) -> Vec<Skill> {
    let roots = crate::ship::roots();
    skill::Roots::new(
        (machine == Machine::Local).then(|| cwd.join(".agents").join("skills")),
        roots.home.join("skills"),
        Some(roots.machine.join("skills")),
    )
    .all()
}

/// One row: what to call it, the path that names it, where it comes from, what it is
/// for, and what to open.
///
/// # Invariant
///
/// `id` is `skill::Skill::id` and nothing else: `ls_skill` filters a `path` against it
/// and `run_skill` hands it back unchanged, so the row a reader copies is the word the
/// call takes.
pub(crate) fn row(skill: &Skill) -> Value {
    json!({
        "id": skill.id(),
        "name": skill.name,
        "description": skill.description,
        "path": skill.path.display().to_string(),
        "bucket": skill.bucket.as_str(),
    })
}
