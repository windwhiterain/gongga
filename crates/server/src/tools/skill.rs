//! Running a skill: a package's request on stdin, or an entry's words.
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
    /// The home's `lib`: the content installs the package contract and the client there.
    pub lib: Option<PathBuf>,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Skills {
    /// Runs one installed skill as `ctx`'s session, with `cmd` as its call.
    ///
    /// # Invariant
    ///
    /// One door for every form, and `run_skill` is the only caller: a package reads one
    /// JSON line on its stdin holding the function and its keyword arguments, an entry
    /// starts the file its frontmatter names with the call as argv, and a body is
    /// refused, naming the `SKILL.md` to read. No shell stands between the words and
    /// the program, so a word arrives as it was written.
    pub async fn run(&self, skill: &Skill, cmd: Option<&Value>, ctx: &ToolContext) -> ToolOutcome {
        // A package skill needs no declared entry: its callable is discovered.
        if skill.entry.is_none() && !skill.dir().join("__init__.py").is_file() {
            return ToolOutcome::err(format!(
                "{} is a body and nothing to run: read {} and do what it says",
                skill.name,
                skill.path.display()
            ));
        }
        let call = match call_of(cmd) {
            Ok(call) => call,
            Err(why) => return ToolOutcome::err(why),
        };
        let launch = match ::skill::runner_of(
            skill.dir(),
            skill.entry.as_deref(),
            &self.programs,
            self.lib.as_deref(),
        ) {
            Ok(launch) => launch,
            Err(why) => return ToolOutcome::err(why),
        };
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
        // The call's shape is the form's: a request on stdin, or the entry's argv.
        let (program, words, request) = match &launch {
            ::skill::Launch::Package(runner) => {
                let arguments = match &call.args {
                    Value::Object(table) => Value::Object(table.clone()),
                    Value::Null => Value::Object(serde_json::Map::new()),
                    other => {
                        return ToolOutcome::err(format!(
                            "a package's function takes named arguments: `args` is an object,                              and this is {}",
                            crate::tools::json_type(other)
                        ));
                    }
                };
                let request = ::skill::request(skill.dir(), &call.function, &arguments);
                (&runner.program, runner.leading.clone(), Some(request))
            }
            ::skill::Launch::Entry(runner) => {
                let argv = match entry_words(&call) {
                    Ok(argv) => argv,
                    Err(why) => return ToolOutcome::err(why),
                };
                let target = skill.entry.as_deref().unwrap_or(skill.dir());
                (&runner.program, runner.words(target, &argv), None)
            }
        };
        let outcome =
            match words::run(program, &words, request.as_deref(), &at, &env, self.timeout).await {
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

/// One `run_skill` call, as SPEC's `Cmd := {name, args}` names it.
struct Call {
    /// The function a package publishes; empty asks it to name its own.
    function: String,
    /// The arguments: an object of named ones, a list of words for an entry, or none.
    args: Value,
}

/// The one sentence a call gets when it names no call at all.
fn needs_a_call() -> String {
    format!(
        "run_skill needs a `cmd`: the function to run and its arguments, `{}`, or one bare word",
        "{\"name\": \"<function>\", \"args\": {}}"
    )
}

/// The call one `run_skill` gives its skill, read off SPEC's `Cmd`.
///
/// # Invariant
///
/// `cmd` is `{name, args}`, and `args` stays what it was written as: nothing here
/// flattens it into a command line -- which reading it gets belongs to the form the
/// skill has, and `entry_words` is the only reader that spells one. A bare word is the
/// whole call, and a string starting with `{` is an object, because a line spells it.
fn call_of(cmd: Option<&Value>) -> Result<Call, String> {
    let Some(value) = cmd.filter(|value| !value.is_null()) else {
        return Err(needs_a_call());
    };
    let value = match value {
        Value::String(text) if !text.trim().starts_with('{') => {
            let word = text.trim();
            return match word.is_empty() {
                true => Err(needs_a_call()),
                false => Ok(Call {
                    function: word.to_string(),
                    args: Value::Null,
                }),
            };
        }
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Err(format!(
                    "`cmd` starts with `{{`, so it is the call as an object of `name` \
                     and `args`, and it does not parse: {e}"
                ));
            }
        },
        other => other.clone(),
    };
    let Value::Object(table) = value else {
        return Err(format!(
            "`cmd` is a call -- an object of `name` and `args` -- and this is {}",
            crate::tools::json_type(&value)
        ));
    };
    let function = table
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    Ok(Call {
        function,
        args: table.get("args").cloned().unwrap_or(Value::Null),
    })
}

/// The words an entry script sees: its first word, then its arguments.
///
/// # Invariant
///
/// An entry is a program with an argv, so its call is a command line: an object is
/// `--key value` per entry, and a list is those words as written. A word is handed over
/// whole -- no shell reads it again -- so a space, a quote or a `$` arrives as typed.
fn entry_words(call: &Call) -> Result<Vec<String>, String> {
    let mut words: Vec<String> = Vec::new();
    if !call.function.is_empty() {
        words.push(call.function.clone());
    }
    match &call.args {
        Value::Null => {}
        Value::Array(items) => {
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
        Value::Object(table) => {
            for (key, value) in table {
                words.push(format!("--{key}"));
                words.push(match value {
                    Value::String(word) => word.clone(),
                    other => other.to_string(),
                });
            }
        }
        other => {
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
        bash: crate::config::detect_git_bash(),
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
