//! Running a skill: a package's request on stdin, else a refusal.
//!
//! See `docs/notes/server-tools-skill.md`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::words;
use crate::tools::{ToolContext, ToolOutcome};
use skill::Skill;

/// How one skill is started: the interpreter, the home's import, and the run's bounds.
pub struct Skills {
    /// The interpreter a package runs under, as the config resolved it.
    pub python: Option<PathBuf>,
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
    /// One form, one door: a skill is a Python package, and its call is one JSON line on its
    /// stdin. A directory without `__init__.py` is refused before a process starts -- a body
    /// naming its `SKILL.md`, a declared entry naming what to change -- because SPEC makes
    /// every skill the model calls a Python call, and `crates/skill/src/entry.rs` keeps the
    /// entry rule for the command line alone.
    pub async fn run(&self, skill: &Skill, cmd: Option<&Value>, ctx: &ToolContext) -> ToolOutcome {
        if !::skill::is_package(skill.dir()) {
            return ToolOutcome::err(refusal(skill));
        }
        let call = match call_of(cmd) {
            Ok(call) => call,
            Err(why) => return ToolOutcome::err(why),
        };
        let runner = match ::skill::package_of(self.python.as_deref(), self.lib.as_deref()) {
            Ok(runner) => runner,
            Err(why) => return ToolOutcome::err(why),
        };
        // The contract and the package are this machine's files, whatever machine the
        // session works on.
        let at = ctx.dir(Machine::Local);
        // The package form's second half: a skill's Python imports the contract.
        let mut env: Vec<(String, String)> = ctx.env.clone();
        if let Some(lib) = self.lib.as_deref() {
            env.push((
                "GONGGA_SKILL_LIB".to_string(),
                lib.to_string_lossy().into_owned(),
            ));
        }
        // A function's arguments are keyword arguments: position was the entry's rule.
        let arguments = match &call.args {
            Value::Object(table) => Value::Object(table.clone()),
            Value::Null => Value::Object(serde_json::Map::new()),
            other => {
                return ToolOutcome::err(format!(
                    "a package's function takes named arguments: `args` is an object, \
                     and this is {}",
                    crate::tools::json_type(other)
                ));
            }
        };
        let request = ::skill::request(skill.dir(), &call.function, &arguments);
        let outcome = match words::run(
            &runner.program,
            &runner.leading,
            Some(&request),
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
        // The skill's own exit code is this call's: a non-zero one is the skill
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

/// The one sentence a skill that is not a package gets.
///
/// # Invariant
///
/// Two shapes, two answers, and neither is about a file being missing: a body is prose the
/// reader opens, and a declared entry is a program the command line runs -- so the sentence
/// names the change that makes the skill callable, which is what a model can act on.
fn refusal(skill: &Skill) -> String {
    match skill.entry.as_deref() {
        None => format!(
            "{} is a body and nothing to run: read {} and do what it says",
            skill.name,
            skill.path.display()
        ),
        Some(entry) => format!(
            "{} declares the entry {}, and this server runs a skill only as a Python call: \
             make it a package -- a directory holding `__init__.py`, whose public functions \
             are the names `run_skill` takes (SPEC's `## Skill`) -- or run the entry from a \
             shell with `gongga-cli skill {}`",
            skill.name,
            entry.display(),
            skill.name
        ),
    }
}

/// One `run_skill` call, as SPEC's `Cmd := {name, args}` names it.
struct Call {
    /// The function a package publishes; empty asks it to name its own.
    function: String,
    /// The function's keyword arguments, an object; `Null` is a call naming none.
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
/// flattens it into a command line, because the call is a JSON line and not an argv. A
/// bare word is the whole call, and a string starting with `{` is an object, because a
/// line spells it.
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
