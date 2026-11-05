//! `skill`: run the entry a skill declares, with its argv and no shell.
//!
//! See `docs/notes/server-tools-skill.md`.

use std::time::Duration;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::index_skill::installed_for;
use crate::tools::words;
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// What `skill` says it is: one entry, its own argv, no shell.
const DESCRIPTION: &str = "Run the entry a skill declares: the program or script its \
     `SKILL.md` names, started with the words you give, and nothing between them -- no \
     shell, so a word holding a space, a quote or a `$` arrives as it is. This is the \
     skill's own answer, not a copy of it: the entry's exit code is this call's `ok`, and \
     what it printed is what comes back. How the entry starts (a shell, Python, \
     PowerShell, or the file itself) is read off its extension and its `#!` line, so name \
     the script's arguments and not its interpreter. `index_skill` lists the names; a \
     skill that is a body with no entry has nothing to run and says so, and its `SKILL.md` \
     is the thing to read instead. The entry runs on this machine, in the session's \
     directory. Use `exec` when the program is yours to name; use this when the skill \
     owns it.";

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
                    "description": "The skill's name: its frontmatter's, or its directory's, \
                                    as `index_skill` lists it."
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The entry's arguments, one word each: passed through \
                                    unparsed and unquoted, so a word holding a space or a \
                                    `$` needs no escaping and gets none."
                }
            },
            "required": ["name"]
        })
    }

    fn run<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(name) = args
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                return ToolOutcome::err(
                    "skill requires a `name`: the skill's own, as `index_skill` lists it"
                        .to_string(),
                );
            };
            let argv = match words_of(&args) {
                Ok(argv) => argv,
                Err(why) => return ToolOutcome::err(why),
            };
            // One reading of the roots: the same rows `index_skill` publishes.
            let found: Vec<::skill::Skill> = installed_for(&ctx.cwd, ctx.machine)
                .into_iter()
                .filter(|skill| skill.name == name)
                .collect();
            let Some(taken) = found.first() else {
                return ToolOutcome::err(format!(
                    "no skill named {name:?} is installed here; `index_skill` lists the ones \
                     that are"
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
            let Some(entry) = &taken.entry else {
                return ToolOutcome::err(format!(
                    "{name} is a body and nothing to run: read {} and do what it says",
                    taken.path.display()
                ));
            };
            let run = match ::skill::runner(entry, &self.programs) {
                Ok(run) => run,
                Err(why) => return ToolOutcome::err(why),
            };
            // The entry is this machine's file whatever machine the session works on.
            let at = ctx.dir(Machine::Local);
            let outcome =
                match words::run(&run.program, &run.words(entry, &argv), &at, self.timeout).await {
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
