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
}

impl Skills {
    /// Runs one installed skill as `ctx`'s session, with `cmd` as its call.
    ///
    /// # Invariant
    ///
    /// One form, one door: a skill is a Python package, and its call is one JSON line on its
    /// stdin. A directory without `__init__.py` is refused before a process starts, because
    /// SPEC makes every skill the model calls a Python call: there is no second form for a
    /// script to be, and no shell door that would run one.
    pub async fn run(&self, skill: &Skill, cmd: Option<&Value>, ctx: &ToolContext) -> ToolOutcome {
        if !::skill::is_package(skill.dir()) {
            return ToolOutcome::err(refusal(skill));
        }
        let call = match call_of(cmd) {
            Ok(call) => call,
            Err(why) => return ToolOutcome::err(why),
        };
        let package = match ::skill::package_of(self.python.as_deref(), self.lib.as_deref()) {
            Ok(package) => package,
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
        // A function's arguments are keyword arguments: a call has a shape, not position.
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
        // The contract is the interpreter's one word: the call itself arrives on stdin.
        let leading = [package.contract.display().to_string()];
        let outcome = match words::run(
            &package.python,
            &leading,
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
        let text = text;
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
/// One shape, one answer, and it is not about a file being missing: the sentence names the
/// change that makes the skill callable -- a package, whose public functions are the names
/// `run_skill` takes -- which is what a model can act on. An implementation that is not
/// Python goes inside one of those functions, because a skill has no other door.
fn refusal(skill: &Skill) -> String {
    format!(
        "{} runs only as a Python call: make it a package -- a directory holding \
         `__init__.py`, whose public functions are the names `run_skill` takes (SPEC's \
         `## Skill`) -- and put an implementation that is not Python inside one of those \
         functions. {} is the prose that describes it.",
        skill.name,
        skill.path.display()
    )
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

/// A skill this build found, and the directory it lives in.
///
/// # Invariant
///
/// `path` is spelled for the machine the skill is on, and is the string a body is read
/// by and a row reports: a project's skill on a peer is a peer path, not this machine's.
///
/// # Invariant
///
/// One spelling for every door, and it is [`crate::space`]'s: this function writes it and
/// [`crate::space::machine_of`] reads it, so no second copy can drift from the first.
/// See `docs/notes/server-meta.md`.
pub fn spelled(machine: Option<&str>, path: &str) -> String {
    crate::space::landed(machine, path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub description: String,
    pub path: String,
    /// The directory holding `SKILL.md`, spelled the same way.
    pub dir: String,
    pub bucket: String,
}

/// Every skill a session standing on `space`'s machine in `cwd` can open.
///
/// # Invariant
///
/// One walk, shared with the command layer and with `@skill(...)`: a name this lists is
/// a name `run_skill` takes. The project layer is the checkout the session stands in,
/// so on a peer it is the peer's own `.agents/skills` read through that peer's space;
/// the home's and the machine's layers are this server's, wherever the session works.
/// See `docs/notes/server-tools-skill.md`.
pub async fn installed_in(space: &crate::space::Space, cwd: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    // The project layer first: the most specific, and the session's own checkout,
    // which may be a peer's.
    let project = space.spell(cwd, ".agents/skills");
    out.extend(read_layer(space, &project, skill::Bucket::Project.as_str()).await);
    // Read here, whatever machine the session works on.
    let here = crate::space::Space::local();
    let roots = crate::ship::roots();
    for (dir, bucket) in [
        (crate::ship::skills(&roots.home), skill::Bucket::Harness),
        (roots.machine.join("skills"), skill::Bucket::General),
    ] {
        let spelled = dir.to_string_lossy().into_owned();
        out.extend(read_layer(&here, &spelled, bucket.as_str()).await);
    }
    out
}

/// The skills whose `SKILL.md` one directory holds, sorted by directory.
///
async fn read_layer(space: &crate::space::Space, dir: &str, bucket: &str) -> Vec<Found> {
    let Some(entries) = space.list(dir).await else {
        return Vec::new();
    };
    // Sorted, so a table drawn twice is the same table.
    let mut dirs: Vec<String> = entries
        .iter()
        .filter(|entry| entry.is_dir)
        .map(|entry| format!("{}/{}", dir.trim_end_matches(['/', '\\']), entry.name))
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for one in dirs {
        let skill_file = format!("{one}/SKILL.md");
        let Some(bytes) = space.read_bytes(&skill_file).await else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let declared = ::skill::declared_of(&text, &one);
        out.push(Found {
            name: declared.0,
            description: declared.1,
            path: spelled(space.name(), &skill_file),
            dir: spelled(space.name(), &one),
            bucket: bucket.to_string(),
        });
    }
    out
}

/// The one skill `name` opens, on the machine `space` is, in the checkout `cwd`.
///
/// # Invariant
///
/// The precedence is `Bucket::ALL`: a project's own skill beats this home's, which
/// beats the machine's, and a name in two layers is the first one read.
pub async fn find(space: &crate::space::Space, cwd: &Path, name: &str) -> Option<Found> {
    installed_in(space, cwd)
        .await
        .into_iter()
        .find(|skill| skill.name == name)
}

/// The same three roots, for a caller with no session to stand in a project root.
pub fn roots(project: Option<&Path>) -> skill::Roots {
    let roots = crate::ship::roots();
    skill::Roots::new(
        project.map(|cwd| cwd.join(".agents").join("skills")),
        // The content's own tree in the checkout: the files a home reads, not copies.
        crate::ship::skills(&roots.home),
        Some(roots.machine.join("skills")),
    )
}

/// One row: what to call it, which layer holds it, what it is for, and what to open.
///
/// # Invariant
///
/// `name` is the whole address and the only thing a call spells: `ls_skill` filters a
/// `path` against it and `run_skill` hands it back unchanged, so the row a reader copies
/// is the word the call takes. `bucket` stands beside it, so a row says where a skill
/// came from without the call having to spell it.
pub(crate) fn found_row(skill: &Found) -> Value {
    json!({
        "name": skill.name,
        "description": skill.description,
        "path": skill.path,
        "bucket": skill.bucket,
    })
}

/// The same row for a skill this machine holds, which is what `roots` reads.
pub(crate) fn row(skill: &Skill) -> Value {
    json!({
        "name": skill.name,
        "description": skill.description,
        "path": skill.path.display().to_string(),
        "bucket": skill.bucket.as_str(),
    })
}
