//! `skill`: run the entry a skill declares, with its argv and no shell.
//!
//! See `docs/notes/server-tools-skill.md`.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::words;
use crate::tools::{Tool, ToolContext, ToolOutcome};
use skill::Skill;

/// What `skill` says it is: one entry, its own argv, no shell.
const DESCRIPTION: &str = "Run the entry a skill declares, or list what is installed.

With no `name`: one row per skill this session can open -- what to call it, the line \
saying what it is for, the `SKILL.md` to read, and which root it came from (`project` for \
this checkout, `harness` for the home, `general` for the machine, in that precedence).

With `name`: that entry, started with your `args` and nothing between them -- no shell, \
so a word holding a space, a quote or a `$` arrives as it is. How it starts (a shell, a \
Python, PowerShell, or the file itself) is read off its extension and its `#!` line: give \
the script's arguments, never its interpreter. The entry's own output comes back and its \
exit code is this call's `ok`, so a non-zero one is the skill failing, not this tool. A \
skill that is a body with no entry refuses, naming the `SKILL.md` to read instead, and a \
name installed in two roots refuses rather than picking one.

It runs on this machine in the session's directory, whatever machine the session works \
on: an entry is a file this server can open. Use `exec` when the program is yours to \
name; use this when the skill owns it.";

pub struct SkillTool {
    /// What an entry may be started by, as the server resolved them.
    pub programs: ::skill::Programs,
    pub timeout: Duration,
    pub max_output_chars: usize,
}

impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill's name, as a row of the list spells it. Omit it \
                                    and this answers with the list instead."
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The entry's arguments, one word each: passed through \
                                    unparsed and unquoted, so a word holding a space or a \
                                    `$` needs no escaping and gets none."
                }
            },
            "required": []
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            // No name is the table: what is installed, with the line that says what
            // each one is for. A name is that entry, run.
            let Some(name) = args
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                return table(ctx);
            };
            let argv = match words_of(&args) {
                Ok(argv) => argv,
                Err(why) => return ToolOutcome::err(why),
            };
            // One reading of the roots, the same walk the table makes.
            let found: Vec<::skill::Skill> = installed_for(&ctx.cwd, ctx.machine)
                .into_iter()
                .filter(|skill| skill.name == name)
                .collect();
            let Some(taken) = found.first() else {
                return ToolOutcome::err(format!(
                    "no skill named {name:?} is installed here; call `skill` with no `name` \
                     for the ones that are"
                ));
            };
            if found.len() > 1 {
                let rows: Vec<String> = found
                    .iter()
                    .map(|skill| format!("{} ({})", skill.dir().display(), skill.bucket.as_str()))
                    .collect();
                return ToolOutcome::err(format!(
                    "{name} is installed more than once and is never resolved silently: {}",
                    rows.join(", ")
                ));
            }
            // A package skill needs no declared entry: its callable is discovered.
            if taken.entry.is_none() && !taken.dir().join("__init__.py").is_file() {
                return ToolOutcome::err(format!(
                    "{name} is a body and nothing to run: read {} and do what it says",
                    taken.path.display()
                ));
            }
            let run = match ::skill::runner_of(taken.dir(), taken.entry.as_deref(), &self.programs)
            {
                Ok(run) => run,
                Err(why) => return ToolOutcome::err(why),
            };
            let target = taken.entry.as_deref().unwrap_or(taken.dir());
            // The entry is this machine's file whatever machine the session works on.
            let at = ctx.dir(Machine::Local);
            let outcome = match words::run(
                &run.program,
                &run.words(target, &argv),
                &at,
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
            let description = args.get("description").and_then(Value::as_str);
            let text = ctx.cap(
                Machine::Local,
                "skill",
                description,
                text,
                self.max_output_chars,
            );
            let code = outcome.code;
            let report = if text.is_empty() {
                format!("[exit status {code}]")
            } else {
                format!("{text}\n[exit status {code}]")
            };
            ToolOutcome {
                output: report,
                ok: code == 0,
            }
        })
    }
}

/// The entry's argv, as the caller gave it: words, and never a command line.
fn words_of(args: &Value) -> Result<Vec<String>, String> {
    match args.get("args") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|word| {
                word.as_str().map(str::to_string).ok_or_else(|| {
                    format!(
                        "skill takes `args` as a list of words, and one of them was {}",
                        crate::tools::json_type(word)
                    )
                })
            })
            .collect(),
        Some(_) => {
            Err("skill takes `args` as a list of words -- a program line is not one".to_string())
        }
    }
}

/// What this server can start an entry with, from the configuration it resolved.
pub fn programs(cfg: &crate::config::Config) -> ::skill::Programs {
    ::skill::Programs {
        bash: cfg.bash.program.clone(),
        python: cfg
            .python
            .program
            .clone()
            .or_else(|| crate::tools::python::detect().ok()),
        powershell: which::which("pwsh")
            .or_else(|_| which::which("powershell"))
            .ok(),
    }
}

/// What is installed for this session, one row per skill.
///
/// # Invariant
///
/// The rows are the walk `installed_for` makes and nothing else, each with the line
/// that says what the skill is for: a reader decides from that line whether to open
/// the body, which is the whole cost this table exists to save.
fn table(ctx: &ToolContext) -> ToolOutcome {
    let skills = installed_for(&ctx.cwd, ctx.machine);
    // A checkout on a peer keeps its skills there: named, not read.
    let elsewhere = (ctx.machine == Machine::Remote).then(|| {
        format!(
            "this session's directory is on its remote endpoint ({}), so the skills of \
             that checkout are that machine's files, under `.agents/skills`. List and \
             read them with `remote_bash` (`ls`, `cat`). The rows above are this \
             machine's -- the home's and the machine's own -- and they are the ones \
             `@skill:` opens.",
            ctx.cwd.display()
        )
    });
    if skills.is_empty() {
        let mut message = String::from(
            "no skill is installed: nothing under the home's `skills/` or the machine's. \
             Say so rather than inventing one.",
        );
        if let Some(elsewhere) = elsewhere {
            message.push('\n');
            message.push_str(&elsewhere);
        }
        return ToolOutcome::ok(message);
    }
    let rows: Vec<Value> = skills.iter().map(row).collect();
    let mut payload = json!({"skills": rows});
    if let Some(elsewhere) = elsewhere {
        payload["elsewhere"] = json!(elsewhere);
    }
    ToolOutcome::ok(serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string()))
}

/// Every skill a session standing on `machine` in `cwd` can open.
///
/// # Invariant
///
/// One walk, shared with the command layer and with `skill <name>`: the project layer
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

/// One row: what to call it, where it comes from, what it is for, and what to open.
fn row(skill: &Skill) -> Value {
    json!({
        "name": skill.name,
        "description": skill.description,
        "path": skill.path.display().to_string(),
        "bucket": skill.bucket.as_str(),
    })
}
