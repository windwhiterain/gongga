//! The skill index: what is installed, where, and what each one is for.
//!
//! See `docs/notes/server-tools-index-skill.md`.

use std::path::Path;

use serde_json::{Value, json};

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
         (`harness` for the skills that speak about this server, installed under \
         the home, or `general` for the machine's own). Call it before deciding \
         a task has no skill, and read a body only when its line matches the \
         task -- reading every skill is the cost this table exists to avoid. \
         Takes no arguments."
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
            let rows = listed(&ctx.cwd);
            if rows.is_empty() {
                return ToolOutcome::ok(
                    "no skill is installed: nothing under the home's `skills/` or the \
                     machine's. Say so rather than inventing one."
                        .to_string(),
                );
            }
            ToolOutcome::ok(
                serde_json::to_string_pretty(&json!({"skills": rows}))
                    .unwrap_or_else(|_| "[]".to_string()),
            )
        })
    }
}

/// One installed skill, as both the table and an `@skill:` reference need it.
///
/// `bucket` is where it was found, which is also what decides precedence.
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
pub fn installed(cwd: &Path) -> Vec<Installed> {
    let roots = crate::ship::roots();
    let mut all = Vec::new();
    for (bucket, root) in [
        // The repository a session is working in. It is the most specific thing
        // that can hold a skill: a checkout that says "here, do it this way".
        (PROJECT, cwd.join(".agents").join("skills")),
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

/// The table's rows: the one walk, in the shape a reader's JSON wants.
fn listed(cwd: &Path) -> Vec<Value> {
    installed(cwd)
        .into_iter()
        .map(|skill| {
            json!({
                "name": skill.name,
                "description": skill.description,
                "path": skill.path.display().to_string(),
                "bucket": skill.bucket,
            })
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn skill(dir: &Path, name: &str, front: &str) -> PathBuf {
        let skill = dir.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), front).unwrap();
        skill
    }

    #[test]
    fn a_skills_line_carries_what_a_reader_needs() {
        let d = tempfile::tempdir().unwrap();
        skill(
            d.path(),
            "heir",
            "---\nname: heir\ndescription: fold a session's context\n---\n\nbody\n",
        );
        let rows: Vec<Value> = in_dir(HARNESS, d.path())
            .iter()
            .map(|s| {
                json!({"name": s.name, "description": s.description, "bucket": s.bucket, "path": s.path.display().to_string()})
            })
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["name"], "heir");
        assert_eq!(rows[0]["description"], "fold a session's context");
        assert_eq!(rows[0]["bucket"], "harness");
        assert!(
            rows[0]["path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("heir/SKILL.md")
                || rows[0]["path"]
                    .as_str()
                    .unwrap_or_default()
                    .ends_with("heir\\SKILL.md"),
            "{rows:?}"
        );
    }

    /// A directory that is not a skill, and a skill that forgot its description:
    /// neither is dropped, and neither is invented.
    #[test]
    fn a_skill_without_frontmatter_is_still_named_by_its_directory() {
        let d = tempfile::tempdir().unwrap();
        skill(d.path(), "plain", "# just prose\n");
        std::fs::create_dir_all(d.path().join("notes")).unwrap();
        let rows: Vec<Value> = in_dir(GENERAL, d.path())
            .iter()
            .map(|s| {
                json!({"name": s.name, "description": s.description, "bucket": s.bucket, "path": s.path.display().to_string()})
            })
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["name"], "plain");
        assert_eq!(rows[0]["description"], "");
        assert_eq!(rows[0]["bucket"], "general");
    }

    /// A session's own directory is a skill root, and the first one.
    ///
    /// # Invariant
    ///
    /// The project's skill is found by `@skill:` and listed in the table, and a
    /// name it shares with the home's resolves to the project's -- a checkout
    /// that says "here, do it this way" outranks the home it sits in.
    #[test]
    fn a_projects_skills_are_found_and_come_first() {
        let project = tempfile::tempdir().unwrap();
        let own = project.path().join(".agents").join("skills");
        skill(
            &own,
            "shared",
            "---\nname: shared\ndescription: the project's\n---\n",
        );
        skill(
            &own,
            "only-here",
            "---\nname: only-here\ndescription: local\n---\n",
        );

        let found = in_dir(PROJECT, &own);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].name, "only-here");
        assert_eq!(found[0].bucket, "project");
        assert_eq!(found[1].description, "the project's");

        // A directory with no `.agents/skills` is not an error: most checkouts
        // have none.
        let bare = tempfile::tempdir().unwrap();
        assert!(in_dir(PROJECT, &bare.path().join(".agents/skills")).is_empty());
    }

    #[test]
    fn an_empty_root_says_there_is_nothing_installed() {
        let d = tempfile::tempdir().unwrap();
        assert!(in_dir(HARNESS, d.path()).is_empty());
        assert!(in_dir(HARNESS, &d.path().join("nowhere")).is_empty());
    }
}
