//! The skill index: what is installed, where, and what each one is for.
//!
//! See `docs/notes/server-tools-index-skill.md`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::{Tool, ToolContext, ToolOutcome};
use skill::Skill;

pub struct IndexSkillTool;

impl Tool for IndexSkillTool {
    fn name(&self) -> &'static str {
        "index_skill"
    }

    fn description(&self) -> &'static str {
        "List every skill this server can offer: its name, the one line saying \
         what it is for, the path of its SKILL.md, and which root it came from \
         (`project` for the checkout this session works in, `harness` for the \
         skills that speak about this server, installed under the home, or \
         `general` for the machine's own). Call it before deciding a task has no \
         skill, and read a body only when its line matches the task -- reading \
         every skill is the cost this table exists to avoid. A session whose \
         directory is on a remote endpoint gets this machine's rows and is told \
         where that checkout's own skills are. Rows come in whatever order keeps \
         this session's request prefix warm; that order ranks nothing. Takes no \
         arguments."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn run<'a>(
        &'a self,
        _args: Value,
        ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let skills = installed_for(&ctx.cwd, ctx.machine);
            // A checkout on a peer keeps its skills there: named, not read.
            let elsewhere = (ctx.machine == Machine::Remote).then(|| {
                format!(
                    "this session's directory is on its remote endpoint ({}), so the \
                     skills of that checkout are that machine's files, under \
                     `.agents/skills`. List and read them with `remote_bash` (`ls`, \
                     `cat`). The rows above are this machine's -- the home's and the \
                     machine's own -- and they are the ones `@skill:` opens.",
                    ctx.cwd.display()
                )
            });
            if skills.is_empty() {
                let mut message = String::from(
                    "no skill is installed: nothing under the home's `skills/` or the \
                     machine's. Say so rather than inventing one.",
                );
                if let Some(elsewhere) = elsewhere {
                    message.push('\n');
                    message.push_str(&elsewhere);
                }
                return ToolOutcome::ok(message);
            }
            let rows = ordered(skills, &ctx.store, &ctx.session, &ctx.model, ctx.rates);
            let mut payload = json!({"skills": rows});
            if let Some(elsewhere) = elsewhere {
                payload["elsewhere"] = json!(elsewhere);
            }
            ToolOutcome::ok(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string()),
            )
        })
    }
}

/// Every skill a session standing on `machine` in `cwd` can open.
///
/// # Invariant
///
/// One walk, shared with the command layer and with `skill <name>`: the project layer
/// is the checkout's, so a session whose directory is on a remote is handed the home's
/// and the machine's rows. See `docs/notes/server-tools-index-skill.md`.
pub fn installed_for(cwd: &Path, machine: Machine) -> Vec<Skill> {
    let roots = crate::ship::roots();
    skill::Roots::new(
        (machine == Machine::Local).then(|| cwd.join(".agents").join("skills")),
        roots.home.join("skills"),
        Some(roots.machine.join("skills")),
    )
    .all()
}

/// The table's rows, in the order this session pays least to send again.
///
/// # Invariant
///
/// A permutation of `skills`: the layout reorders the table and never revives a row
/// for a skill that is not installed now. See `docs/notes/server-prefix-cache.md`.
fn ordered(
    skills: Vec<Skill>,
    store: &crate::store::Store,
    session: &str,
    model: &str,
    rates: crate::prefix_cache::Rates,
) -> Vec<Value> {
    let rows: Vec<(String, String, Value)> = skills
        .into_iter()
        .map(|skill| {
            // The path: two directories may declare one name.
            let key = skill.path.display().to_string();
            let row = json!({
                "name": skill.name,
                "description": skill.description,
                "path": skill.path.display().to_string(),
                "bucket": skill.bucket.as_str(),
            });
            let text = row.to_string();
            (key, text, row)
        })
        .collect();
    // The tokens: what sending this row costs, from what the provider counted.
    let ratio = store.token_ratio(model);
    let wanted: Vec<crate::prefix_cache::Wanted> = rows
        .iter()
        .map(|(key, text, _)| crate::prefix_cache::Wanted {
            key: key.as_str(),
            text: text.as_str(),
            tokens: (text.len() as f64 * ratio).round().max(1.0) as usize,
        })
        .collect();
    // No tolerance: a row is json, and a diff of one is not something a reader reads.
    let planned = crate::prefix_cache::plan(store, session, "skill table", &wanted, rates, None);
    let mut by_key: HashMap<String, Value> =
        rows.into_iter().map(|(key, _, row)| (key, row)).collect();
    planned
        .entries
        .into_iter()
        .filter_map(|(key, _)| by_key.remove(&key))
        .collect()
}
