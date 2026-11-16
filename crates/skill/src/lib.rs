//! What a skill is on disk, and where a session reads one.
//!
//! See `docs/notes/skill-lib.md`.

use std::path::{Path, PathBuf};

pub mod package;

pub use package::{Package, is_package, package_of, request};

/// The file that makes a directory a skill, and the one a reader opens.
const SKILL: &str = "SKILL.md";

/// Which root a skill came from, in the words a reader already knows.
///
/// # Invariant
///
/// `ALL` is the precedence order, and it is the same one the server expands
/// `@skill(...)` with and the command layer completes against: a name in more than one
/// root is the first one read, so a project's own skill beats the home's, which
/// beats the machine's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// The checkout the session works in.
    Project,
    /// This harness's own content, installed under its home by the pin.
    Harness,
    /// The machine's, which is the community clone linked into `~/.agents/skills`.
    General,
}

impl Bucket {
    /// Every bucket, most specific first: the order a name is resolved in.
    pub const ALL: [Bucket; 3] = [Bucket::Project, Bucket::Harness, Bucket::General];

    /// The word a table's `bucket` column says, and the one a prompt already uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Bucket::Project => "project",
            Bucket::Harness => "harness",
            Bucket::General => "general",
        }
    }
}

/// The three directories a session may take a skill from, each one holding
/// skill directories rather than being one.
#[derive(Debug, Clone)]
pub struct Roots {
    /// `<cwd>/.agents/skills`; `None` when that checkout is another machine's.
    pub project: Option<PathBuf>,
    /// `<home>/skills`, installed from the pin.
    pub home: PathBuf,
    /// `~/.agents/skills`; `None` when this machine cannot say where home is.
    pub machine: Option<PathBuf>,
}

impl Roots {
    /// The roots of one session: its own directory, its home, its machine.
    pub fn new(project: Option<PathBuf>, home: PathBuf, machine: Option<PathBuf>) -> Self {
        Self {
            project,
            home,
            machine,
        }
    }

    /// Where `bucket` keeps its skills, when that layer is on this disk.
    fn dir(&self, bucket: Bucket) -> Option<&Path> {
        match bucket {
            Bucket::Project => self.project.as_deref(),
            Bucket::Harness => Some(&self.home),
            Bucket::General => self.machine.as_deref(),
        }
    }

    /// Every skill installed, in precedence order.
    pub fn all(&self) -> Vec<Skill> {
        let mut all = Vec::new();
        for bucket in Bucket::ALL {
            let Some(root) = self.dir(bucket) else {
                continue;
            };
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            // Sorted, so a table drawn twice is the same table.
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|dir| dir.join(SKILL).is_file())
                .collect();
            dirs.sort();
            all.extend(dirs.iter().map(|dir| declared(bucket, dir)));
        }
        all
    }

    /// Every skill a name opens, most specific first: the first is the one that runs.
    pub fn named(&self, name: &str) -> Vec<Skill> {
        self.all()
            .into_iter()
            .filter(|skill| skill.name == name)
            .collect()
    }

    /// The skill one name opens: the nearer layer's, the row every caller takes.
    ///
    /// # Invariant
    ///
    /// The name is the whole address, and [`Bucket::ALL`] is its precedence.
    /// A layer is a field of the row, so a spelling that carries one names no skill.
    /// Nothing here reads a path: a root is one directory of skills, and how deep a
    /// layer keeps its own files is not part of what a caller says.
    ///
    /// See `docs/notes/skill-lib.md`.
    pub fn find(&self, name: &str) -> Option<Skill> {
        self.named(name).into_iter().next()
    }
}

/// One skill directory: what its frontmatter declares, and where its files are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// What `@skill(...)` names it by: the frontmatter's, or the directory's.
    pub name: String,
    /// The one line saying what it is for; empty when the file declares none.
    pub description: String,
    /// The `SKILL.md` itself, which is what a reader opens.
    pub path: PathBuf,
    pub bucket: Bucket,
}

impl Skill {
    /// The directory the skill's files live in: where `package.rs` finds its package.
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// What a caller names it by: its own name.
    ///
    /// # Invariant
    ///
    /// The name is the skill, and where it came from is a field rather than a prefix:
    /// a caller who has a name does not have to know which root holds it, and the root
    /// a name came from is still on the row (`bucket`). Two roots spelling one name are
    /// two rows that answer to one word, which is what [`Roots::find`] and `at` decide
    /// by precedence.
    pub fn id(&self) -> String {
        self.name.clone()
    }
}

/// Reads one skill directory's frontmatter, falling back to its directory name.
fn declared(bucket: Bucket, dir: &Path) -> Skill {
    let path = dir.join(SKILL);
    let meta = frontmatter(&path);
    Skill {
        name: meta.name.unwrap_or_else(|| dir_name(dir)),
        description: meta.description,
        path,
        bucket,
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What the header block declares: the two fields the index needs.
#[derive(Debug, Default)]
struct Meta {
    name: Option<String>,
    description: String,
}

/// `---`, then top-level `key: value` lines, then `---`.
///
/// # Invariant
///
/// Only top-level keys are read, and only these two: an indented line belongs to the key
/// above it (`metadata:` and its fields), which is the reader's or another harness's, not
/// this function's.
fn frontmatter(file: &Path) -> Meta {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Meta::default();
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Meta::default();
    }
    let mut meta = Meta::default();
    for line in lines.by_ref().take_while(|line| line.trim() != "---") {
        if line.len() != line.trim_start().len() {
            continue;
        }
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "name" if !value.is_empty() => meta.name = Some(value.to_string()),
            "description" => meta.description = value.to_string(),
            _ => {}
        }
    }
    meta
}

/// A path the way a POSIX shell reads it: `C:\a\b` is `/c/a/b`, and a POSIX path is itself.
pub fn shell_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("/{}{}", drive.to_ascii_lowercase(), &text[2..])
        }
        _ => text,
    }
}
