//! The completion source the TUI asks, and nowhere else.
//!
//! See `docs/notes/cli-catalog.md`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::complete::{Candidate, Completer, token_bounds};
use protocol::{ArgSpec, CommandSpec, CommandTarget, CompletionSource, Reference};

/// A catalog that can be swapped while the TUI holds a reference to it.
#[derive(Clone, Default)]
pub struct Catalog(Arc<RwLock<Inner>>);

struct Inner {
    /// Everything the server published, as published: this client keeps no
    /// command list of its own.
    published: Vec<CommandSpec>,
    sessions: Vec<String>,
    /// The client's sticky argument names, for the sources the server names
    /// but cannot fill in.
    sticky_names: Vec<String>,
    /// The skills installed on this machine, for an `@skill:` reference.
    skill_names: Vec<String>,
    /// Where a relative path value is read from: the client's own directory.
    cwd: PathBuf,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            published: Vec::new(),
            sessions: Vec::new(),
            sticky_names: Vec::new(),
            skill_names: Vec::new(),
            cwd: std::env::current_dir().unwrap_or_default(),
        }
    }
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the catalog with the one the greeting carried.
    pub fn set_commands(&self, commands: Vec<CommandSpec>) {
        self.write().published = commands;
    }

    pub fn set_sessions(&self, sessions: Vec<String>) {
        self.write().sessions = sessions;
    }

    /// The directory this client reads relative paths from, which is also the
    /// project whose own `.agents/skills` counts.
    pub fn cwd(&self) -> PathBuf {
        self.read().cwd.clone()
    }

    /// The sticky arguments that exist, for the source the server names.
    pub fn set_sticky_names(&self, names: &[String]) {
        self.write().sticky_names = names.to_vec();
    }

    /// The skills installed here, for the source the server names.
    pub fn set_skill_names(&self, names: &[String]) {
        self.write().skill_names = names.to_vec();
    }

    pub fn commands(&self) -> Vec<CommandSpec> {
        self.read().published.clone()
    }

    pub fn command(&self, name: &str) -> Option<CommandSpec> {
        self.read()
            .published
            .iter()
            .find(|c| c.name == name)
            .cloned()
    }

    /// Whether some published command declares an argument by this name as one a
    /// sticky value would fill.
    ///
    /// # Invariant
    ///
    /// `None` when no catalog has been published yet: that cannot prove a name
    /// wrong, and a client that has not connected must still store one.
    pub fn fills_sticky(&self, name: &str) -> Option<bool> {
        let published = self.read().published.clone();
        if published.is_empty() {
            return None;
        }
        Some(published.iter().any(|command| {
            command
                .args
                .iter()
                .any(|a| a.name == name && (a.sticky || a.required))
        }))
    }

    /// The names a sticky value could ever fill, sorted, for a refusal to name.
    pub fn sticky_names(&self) -> Vec<String> {
        let published = self.read().published.clone();
        let mut names: Vec<String> = published
            .iter()
            .flat_map(|command| command.args.iter())
            .filter(|a| a.sticky || a.required)
            .map(|a| a.name.clone())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Whether the caller runs this one itself. Answered from the published
    /// target, never from a list here.
    pub fn is_client_command(&self, name: &str) -> bool {
        self.command(name)
            .map(|c| c.target == CommandTarget::Client)
            .unwrap_or(false)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// What a value may be completed against, snapshotted so no lock is held.
struct Ctx<'a> {
    sessions: &'a [String],
    sticky: &'a [String],
    skills: &'a [String],
    cwd: &'a Path,
    all: &'a [CommandSpec],
}

impl Completer for Catalog {
    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
        let (_, _, token) = token_bounds(line, cursor);
        let (all, sessions, sticky, skills, cwd) = {
            let inner = self.read();
            (
                inner.published.clone(),
                inner.sessions.clone(),
                inner.sticky_names.clone(),
                inner.skill_names.clone(),
                inner.cwd.clone(),
            )
        };
        let ctx = Ctx {
            sessions: &sessions,
            sticky: &sticky,
            skills: &skills,
            cwd: &cwd,
            all: &all,
        };
        // `/` starts a command; a message completes only its `@` references.
        // See `docs/notes/cli-catalog.md`.
        if !line.trim_start().starts_with('/') {
            return reference(&ctx, line, cursor, &token);
        }
        let words = shell_words::split(line).unwrap_or_default();
        let position = line[..cursor.min(line.len())].split_whitespace().count();
        let cmd = words.first().map(|t| t.trim_start_matches('/'));
        // The word the cursor is in is about to be replaced, so it is not
        // already on the line.
        let word_at_cursor = if token.is_empty() {
            None
        } else {
            position.checked_sub(1)
        };

        if let Some(prefix) = token.strip_prefix("--") {
            let args: &[ArgSpec] = cmd
                .and_then(|c| all.iter().find(|s| s.name == c))
                .map(|c| c.args.as_slice())
                .unwrap_or(&[]);
            let used = flags_on_line(&words, word_at_cursor);
            return args
                .iter()
                // A flag already on the line is not a candidate; listing it
                // again hides the flags still missing.
                .filter(|a| !used.contains(a.name.as_str()))
                .filter(|a| a.name.starts_with(prefix))
                .map(|a| Candidate::word(format!("--{}", a.name), a.description.clone()))
                .collect();
        }

        // Command names, matched anywhere in the name: `/render` is `set_render`.
        if position == 1 && token.starts_with('/') {
            let typed = token.trim_start_matches('/');
            let mut got: Vec<Candidate> = all
                .iter()
                .filter(|c| c.name.contains(typed))
                .map(|c| Candidate::word(format!("/{}", c.name), c.description.clone()))
                .collect();
            // A prefix match is what the operator is most likely typing; stable,
            // so the catalog's order decides between equals.
            got.sort_by_key(|c| !c.insert.trim_start_matches('/').starts_with(typed));
            return got;
        }

        // Past the command name: a flag's value, whose flag sits one word back,
        // or two once the value has started.
        let flag_index = if token.is_empty() {
            position.checked_sub(1)
        } else {
            position.checked_sub(2)
        };
        let Some(flag) = flag_index
            .and_then(|i| words.get(i))
            .and_then(|w| w.strip_prefix("--"))
        else {
            return Vec::new();
        };
        let Some(spec) = cmd
            .and_then(|c| all.iter().find(|s| s.name == c))
            .and_then(|c| c.args.iter().find(|a| a.name == flag))
        else {
            return Vec::new();
        };
        resolve(&spec.completions, &token, &ctx, &words, 0)
    }
}

/// Completes a message's `@image:`, `@skill:` and `@file:` references.
///
/// # Invariant
///
/// A reference is completed in two steps that never overlap: the prefix, while one
/// is still being typed, and then the value it names -- a path for a picture or a
/// file, a skill's name for a skill.
fn reference(ctx: &Ctx, line: &str, cursor: usize, token: &str) -> Vec<Candidate> {
    let (start, _, _) = token_bounds(line, cursor);
    if let Some((kind, typed)) = split_reference(token) {
        // `@image:pa`, written together: the prefix is already behind the cursor.
        return value(kind, typed, ctx);
    }
    let previous = line[..start.min(line.len())].split_whitespace().last();
    if let Some((kind, _)) = previous.and_then(split_reference) {
        return value(kind, token, ctx);
    }
    // `@`, `@im`: the three prefixes themselves, and nothing else.
    let Some(bare) = token.strip_prefix('@') else {
        return Vec::new();
    };
    Reference::ALL
        .into_iter()
        .filter(|kind| kind.prefix()[1..].contains(bare))
        .map(|kind| Candidate::word(kind.prefix(), reference_detail(kind)))
        .collect()
}

/// What a reference is for, said in the popup's right-hand column.
fn reference_detail(kind: Reference) -> &'static str {
    match kind {
        Reference::Image => "attach a picture",
        Reference::Skill => "fetch a skill's metadata",
        Reference::File => "mention a file",
    }
}

/// The value part of `token` when it opens a reference, and the kind it opens.
fn split_reference(token: &str) -> Option<(Reference, &str)> {
    let kind = Reference::of_prefix(token)?;
    Some((kind, &token[kind.prefix().len()..]))
}

/// Completes one reference's value.
fn value(kind: Reference, typed: &str, ctx: &Ctx) -> Vec<Candidate> {
    match kind {
        Reference::Skill => pick(ctx.skills.iter().map(String::as_str), typed, "skill"),
        Reference::Image | Reference::File => paths(ctx.cwd, typed),
    }
}

/// Turns one argument's declared source into candidates; `depth` bounds the one
/// self-referential case.
fn resolve(
    source: &CompletionSource,
    token: &str,
    ctx: &Ctx,
    words: &[String],
    depth: u8,
) -> Vec<Candidate> {
    match source {
        CompletionSource::None => Vec::new(),
        CompletionSource::Sessions => {
            pick(ctx.sessions.iter().map(String::as_str), token, "session")
        }
        CompletionSource::Static(values) => pick(values.iter().map(String::as_str), token, "value"),
        CompletionSource::Arguments => {
            let mut names: Vec<&str> = ctx
                .all
                .iter()
                .flat_map(|c| c.args.iter().map(|a| a.name.as_str()))
                .collect();
            names.sort_unstable();
            names.dedup();
            pick(names.into_iter(), token, "argument")
        }
        CompletionSource::Paths => paths(ctx.cwd, token),
        CompletionSource::StickyArguments => {
            pick(ctx.sticky.iter().map(String::as_str), token, "argument")
        }
        CompletionSource::Skills => pick(ctx.skills.iter().map(String::as_str), token, "skill"),
        CompletionSource::OfNamedArgument if depth < 2 => {
            let Some(named) = flag_value(words, "name") else {
                return Vec::new();
            };
            let shaped: Vec<&protocol::ArgSpec> = ctx
                .all
                .iter()
                .flat_map(|c| c.args.iter())
                .filter(|a| {
                    a.name == named
                        && !matches!(
                            a.completions,
                            CompletionSource::None | CompletionSource::OfNamedArgument
                        )
                })
                .collect();
            // The required spelling of a name is its target: that is what a
            // mirrored value completes against.
            let found = shaped
                .iter()
                .find(|a| a.required)
                .or_else(|| shaped.first())
                .map(|a| a.completions.clone());
            match found {
                Some(source) => resolve(&source, token, ctx, words, depth + 1),
                None => Vec::new(),
            }
        }
        CompletionSource::OfNamedArgument => Vec::new(),
    }
}

/// The candidates that carry `token` anywhere, not only at the front.
///
/// # Invariant
///
/// A word already typed keeps its place in the list: an exact match sorts first,
/// so Tab on `/send_message` completes nothing rather than reordering the popup.
/// See `docs/notes/cli-catalog.md`.
fn pick<'a>(values: impl Iterator<Item = &'a str>, token: &str, detail: &str) -> Vec<Candidate> {
    let mut got: Vec<Candidate> = values
        .filter(|v| v.contains(token))
        .map(|v| Candidate::word(v, detail))
        .collect();
    // Stable sort, so the catalog's own order decides between equals.
    got.sort_by_key(|c| !c.insert.starts_with(token));
    got
}

/// Flags the line already carries, in either spelling; `skip` is the word being
/// completed.
fn flags_on_line(words: &[String], skip: Option<usize>) -> std::collections::HashSet<&str> {
    words
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != skip)
        .filter_map(|(_, word)| word.strip_prefix("--"))
        .map(|flag| flag.split('=').next().unwrap_or(flag))
        .collect()
}

/// The value written after `--flag` on this line, in either spelling.
fn flag_value<'a>(words: &'a [String], flag: &str) -> Option<&'a str> {
    let dashed = format!("--{flag}");
    for (i, word) in words.iter().enumerate() {
        if let Some(value) = word.strip_prefix(&dashed).and_then(|r| r.strip_prefix('=')) {
            return Some(value);
        }
        if *word == dashed {
            return words.get(i + 1).map(String::as_str);
        }
    }
    None
}

/// Lists the filesystem under `cwd`, keeping the directory part already typed; a
/// directory keeps its trailing `/`.
fn paths(cwd: &Path, token: &str) -> Vec<Candidate> {
    let (typed_dir, prefix) = match token.rfind(['/', '\\']) {
        Some(at) => token.split_at(at + 1),
        None => ("", token),
    };
    let Ok(entries) = std::fs::read_dir(directory(cwd, typed_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            // Dotfiles stay out of the way until the user asks for them, the
            // way a shell behaves.
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }
            if !name.starts_with(prefix) {
                return None;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                Some(Candidate {
                    insert: format!("{typed_dir}{name}/"),
                    detail: "dir".into(),
                    space: false,
                })
            } else {
                Some(Candidate::word(format!("{typed_dir}{name}"), "file"))
            }
        })
        .collect();
    out.sort_by(|a, b| a.insert.cmp(&b.insert));
    out
}

/// The directory `typed` names, relative to the client's, with `~` expanded.
fn directory(cwd: &Path, typed: &str) -> PathBuf {
    let path = match typed.strip_prefix('~') {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(typed),
        },
        None => PathBuf::from(typed),
    };
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}
