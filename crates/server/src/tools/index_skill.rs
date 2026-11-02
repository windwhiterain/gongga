//! The skill index: what is installed, where, and what each one is for.
//!
//! See `docs/notes/server-tools-index-skill.md`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::session::Machine;
use crate::tools::{Tool, ToolContext, ToolOutcome};

/// Which root a skill came from, in the words a reader already knows.
///
/// # Invariant
///
/// The order is the precedence: a name in more than one root is the first one
/// read, so a project's own skill beats the home's, which beats the machine's.
const PROJECT: &str = "project";
const HARNESS: &str = "harness";
const GENERAL: &str = "general";

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
            let rows = ordered(
                skills,
                &ctx.store,
                &ctx.session,
                &ctx.model,
                ctx.price,
                ctx.cache_invalid_rate,
                ctx.cache_ttl_ms,
            );
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

/// One installed skill: the table's row, and what `@skill:` opens.
#[derive(Debug)]
pub struct Installed {
    pub name: String,
    pub description: String,
    pub path: std::path::PathBuf,
    pub bucket: &'static str,
}

/// Every skill a session in `cwd` can be offered: the project's, the home's,
/// then the machine's.
///
/// # Invariant
///
/// The order is the precedence, and it is the same order `installed_skills` walks
/// for the command line, so a name read off the table is a name that can be
/// opened. See `docs/notes/server-tools-index-skill.md`.
/// Every skill a session standing on `machine` in `cwd` can open.
///
/// # Invariant
///
/// Only a row whose file this server can read: the project layer is the checkout's,
/// so a session whose directory is on a remote is handed the home's and the
/// machine's rows and told where its own checkout's skills are.
pub fn installed_for(cwd: &Path, machine: Machine) -> Vec<Installed> {
    let roots = crate::ship::roots();
    let mut all = Vec::new();
    if machine == Machine::Local {
        // The checkout the session is in: the most specific holder of a skill.
        all.extend(in_dir(PROJECT, &cwd.join(".agents").join("skills")));
    }
    for (bucket, root) in [
        (HARNESS, roots.home.join("skills")),
        (GENERAL, roots.machine.join("skills")),
    ] {
        all.extend(in_dir(bucket, &root));
    }
    all
}

/// One directory's skills, in name order so two calls read the same.
fn in_dir(bucket: &'static str, root: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("SKILL.md").is_file())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|dir| {
            let path = dir.join("SKILL.md");
            let (name, description) = frontmatter(&path);
            Installed {
                name: name.unwrap_or_else(|| dir_name(dir)),
                description,
                path,
                bucket,
            }
        })
        .collect()
}

/// The table's rows, in the order this session pays least to send again.
///
/// # Invariant
///
/// A permutation of `skills`: the layout reorders the table and never revives a row
/// for a skill that is not installed now. See `docs/notes/server-prefix-cache.md`.
fn ordered(
    skills: Vec<Installed>,
    store: &crate::store::Store,
    session: &str,
    model: &str,
    price: Option<crate::providers::Price>,
    invalid: f64,
    ttl_ms: u64,
) -> Vec<Value> {
    let rows: Vec<(String, Value)> = skills
        .into_iter()
        .map(|skill| {
            // The path: two directories may declare one name.
            let key = skill.path.display().to_string();
            let row = json!({
                "name": skill.name,
                "description": skill.description,
                "path": skill.path.display().to_string(),
                "bucket": skill.bucket,
            });
            (key, row)
        })
        .collect();
    // The tokens and their digest: an edited row is a new item.
    let ratio = store.token_ratio(model);
    let rates = crate::prefix_cache::Rates {
        price,
        invalid,
        ttl_ms,
    };
    let wanted: Vec<(&str, usize, u64)> = rows
        .iter()
        .map(|(key, row)| {
            let text = row.to_string();
            let digest = crate::prefix_cache::digest(&text);
            let tokens = (text.len() as f64 * ratio).round().max(1.0) as usize;
            (key.as_str(), tokens, digest)
        })
        .collect();
    let order = crate::prefix_cache::plan(store, session, "skill table", &wanted, rates);
    let mut by_key: HashMap<String, Value> = rows.into_iter().collect();
    order
        .into_iter()
        .filter_map(|key| by_key.remove(&key))
        .collect()
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The `name` and `description` of a skill's frontmatter; either may be absent.
fn frontmatter(file: &Path) -> (Option<String>, String) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return (None, String::new());
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, String::new());
    }
    let mut name = None;
    let mut description = String::new();
    for line in lines.by_ref().take_while(|l| l.trim() != "---") {
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = rest.trim().trim_matches('"').to_string();
        }
    }
    (name.filter(|n| !n.is_empty()), description)
}
