//! The skill index: what is installed, where, and what each one is for.
//!
//! See `docs/notes/server-tools-index-skill.md`.

use std::path::Path;

use serde_json::{Value, json};

use crate::tools::{Tool, ToolContext, ToolOutcome};

/// Which root a skill came from, in the words a reader already knows.
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
         the home, or `general` for the machine's own: the community clone under \
         `~/.agents/gongga-agents/skills`, then the private `~/.agents/skills`, \
         which wins a name they share). Call it before deciding a task has no \
         skill, and read a body only when its line matches the task -- reading \
         every skill is the cost this table exists to avoid. Takes no arguments."
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
        _ctx: &'a ToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let roots = crate::ship::roots();
            let agents = crate::agents::Roots::of();
            let mut rows = listed(HARNESS, &roots.home.join("skills"));
            rows.extend(general(&agents.community, &roots.machine));
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

/// The general bucket: the community clone, then the machine's own copy.
///
/// # Invariant
///
/// A name the two roots share is the private one, the same way a private
/// `providers.toml` overrides the community entry: the community list is the
/// floor, and nothing here silently shows a reader two skills by one name.
fn general(community: &Path, private: &Path) -> Vec<Value> {
    let mut rows = listed(GENERAL, &community.join("skills"));
    for row in listed(GENERAL, &private.join("skills")) {
        let name = row["name"].clone();
        match rows.iter().position(|seen| seen["name"] == name) {
            Some(at) => rows[at] = row,
            None => rows.push(row),
        }
    }
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    rows
}

/// One directory's skills, in name order so two calls read the same.
fn listed(bucket: &str, root: &Path) -> Vec<Value> {
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
            let file = dir.join("SKILL.md");
            let (name, description) = frontmatter(&file);
            json!({
                "name": name.unwrap_or_else(|| dir_name(dir)),
                "description": description,
                "path": file.display().to_string(),
                "bucket": bucket,
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
        let rows = listed(HARNESS, d.path());
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
        let rows = listed(GENERAL, d.path());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["name"], "plain");
        assert_eq!(rows[0]["description"], "");
        assert_eq!(rows[0]["bucket"], "general");
    }

    #[test]
    fn an_empty_root_says_there_is_nothing_installed() {
        let d = tempfile::tempdir().unwrap();
        assert!(listed(HARNESS, d.path()).is_empty());
        assert!(listed(HARNESS, &d.path().join("nowhere")).is_empty());
    }
}
