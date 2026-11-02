//! The completion source the TUI asks, and nowhere else.
//!
//! See `docs/notes/cli-catalog.md`.

use std::collections::{HashMap, HashSet};
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
    /// Which machine each session's shells run on, as the server published it.
    ///
    /// # Invariant
    ///
    /// Kept beside the names, never derived: a value completed for a session belongs
    /// to that session's machine, and the line does not have to repeat `--target`.
    session_targets: HashMap<String, String>,
    /// The client's sticky argument names, for the sources the server names
    /// but cannot fill in.
    sticky_names: Vec<String>,
    /// The skills installed on this machine, for an `@skill:` reference.
    skill_names: Vec<String>,
    /// Where a relative path value is read from: the client's own directory.
    cwd: PathBuf,
    /// Directories a target answered with, keyed by target and directory, and
    /// when each answer arrived.
    remote_paths: HashMap<(String, String), (Vec<Candidate>, std::time::Instant)>,
    /// The names a config holds, for reading `--target` off the line. Taken from
    /// the published catalog rather than kept here.
    targets: HashSet<String>,
    /// The directory each target's line last asked for and has no answer for.
    ///
    /// # Invariant
    ///
    /// One entry per target, never one per directory: the line is completed on
    /// every keystroke, so a queue keyed by directory asks for a listing per letter
    /// typed, answers them in order, and is always behind the cursor.
    wanted: HashMap<String, String>,
    /// The directory each target is being asked for right now, so a second
    /// question never queues behind the first.
    inflight: HashMap<String, String>,
    /// Bumped whenever `remote_paths` changes, so a front-end repaints the popup.
    paths_version: u64,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            published: Vec::new(),
            sessions: Vec::new(),
            session_targets: HashMap::new(),
            sticky_names: Vec::new(),
            skill_names: Vec::new(),
            cwd: std::env::current_dir().unwrap_or_default(),
            remote_paths: HashMap::new(),
            targets: HashSet::new(),
            wanted: HashMap::new(),
            inflight: HashMap::new(),
            paths_version: 0,
        }
    }
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the catalog with the one the greeting carried.
    pub fn set_commands(&self, commands: Vec<CommandSpec>) {
        // Any argument named `target` publishes exactly the names a config holds,
        // so the client reads them off the catalog instead of the config file.
        let names: HashSet<String> = commands
            .iter()
            .flat_map(|command| command.args.iter())
            .filter(|arg| arg.name == "target")
            .flat_map(|arg| match &arg.completions {
                CompletionSource::Static(values) => values.clone(),
                _ => Vec::new(),
            })
            .collect();
        self.write().targets = names;
        self.write().published = commands;
    }

    pub fn set_sessions(&self, sessions: Vec<String>, targets: HashMap<String, String>) {
        let mut inner = self.write();
        inner.sessions = sessions;
        inner.session_targets = targets;
    }

    /// The directory this client reads relative paths from, which is also the
    /// project whose own `.agents/skills` counts.
    /// The listings a completion asked for and does not have yet, as
    /// `(target, directory)`; taking them is what stops a repeat ask.
    pub fn take_wanted(&self) -> Vec<(String, String)> {
        let mut inner = self.write();
        let asked = std::mem::take(&mut inner.wanted);
        let mut sending = Vec::new();
        for (target, directory) in asked {
            // Already asked: what is missing is the answer, not the question.
            if inner.inflight.get(&target) == Some(&directory) {
                continue;
            }
            inner.inflight.insert(target.clone(), directory.clone());
            sending.push((target, directory));
        }
        sending
    }

    /// Records what a target answered, so the next completion offers it.
    pub fn fill_paths(&self, target: &str, directory: &str, entries: Vec<(String, bool)>) {
        let typed = directory.to_string();
        let named: Vec<Candidate> = entries
            .into_iter()
            .map(|(name, is_dir)| {
                if is_dir {
                    // Descending is what a directory is for, so no space follows.
                    Candidate {
                        insert: format!("{typed}{name}/"),
                        detail: "dir".into(),
                        space: false,
                    }
                } else {
                    Candidate::word(format!("{typed}{name}"), "file")
                }
            })
            .collect();
        let mut inner = self.write();
        // This target may be asked again, and the line says what it now wants.
        if inner.inflight.get(target).map(String::as_str) == Some(directory) {
            inner.inflight.remove(target);
        }
        inner.remote_paths.insert(
            (target.to_string(), directory.to_string()),
            (named, std::time::Instant::now()),
        );
        inner.paths_version += 1;
    }

    /// How many listings have arrived; a front-end repaints when this changes.
    pub fn paths_version(&self) -> u64 {
        self.read().paths_version
    }

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

/// What a value may be completed against, snapshotted so no lock is held --
/// except the listing cache, which is read on the one branch that needs it.
struct Ctx<'a> {
    sessions: &'a [String],
    sticky: &'a [String],
    skills: &'a [String],
    cwd: &'a Path,
    all: &'a [CommandSpec],
    /// The target names a config holds, as the catalog published them.
    targets: &'a HashSet<String>,
    catalog: &'a Catalog,
}

impl Completer for Catalog {
    fn wants(&self) -> bool {
        !self.read().wanted.is_empty()
    }

    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
        let (_, _, token) = token_bounds(line, cursor);
        let (all, sessions, sticky, skills, cwd, targets) = {
            let inner = self.read();
            (
                inner.published.clone(),
                inner.sessions.clone(),
                inner.sticky_names.clone(),
                inner.skill_names.clone(),
                inner.cwd.clone(),
                inner.targets.clone(),
            )
        };
        let ctx = Ctx {
            sessions: &sessions,
            sticky: &sticky,
            skills: &skills,
            cwd: &cwd,
            all: &all,
            targets: &targets,
            catalog: self,
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
        CompletionSource::Paths => match named_target(ctx, words) {
            // The line runs on another machine, so its paths are that machine's.
            Some(target) => remote_paths(ctx, &target, token),
            None => paths(ctx.cwd, token),
        },
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

/// How long an answer stands before the same directory is asked for again.
///
/// # Invariant
///
/// Above zero, because a repaint re-asks: an answer that were always stale would
/// turn every refresh into another peer round-trip, and the refresh into a loop.
const REVALIDATE: std::time::Duration = std::time::Duration::from_secs(1);

/// The target this line belongs to, if it names one the config holds.
///
/// # Invariant
///
/// Two ways to name a machine, in this order: `--target` on the line, then the
/// target of the `--session` the line names. The second is what makes a path
/// complete on the machine it will be read on without the operator repeating
/// `--target` for a session that already says where it is.
fn named_target(ctx: &Ctx, words: &[String]) -> Option<String> {
    if let Some(asked) = flag_value(words, "target") {
        if ctx.targets.contains(asked) {
            return Some(asked.to_string());
        }
    }
    let session = flag_value(words, "session")?;
    let recorded = ctx.catalog.read().session_targets.get(session).cloned()?;
    ctx.targets.contains(&recorded).then_some(recorded)
}

/// Completes `--cwd` against the filesystem of the machine the line names.
///
/// # Invariant
///
/// Never waits for a peer: what has arrived is offered now, and the rest is asked
/// for, so a completion never blocks the front-end on a network.
fn remote_paths(ctx: &Ctx, target: &str, token: &str) -> Vec<Candidate> {
    let (typed_dir, prefix) = match token.rfind(['/', '\\']) {
        Some(at) => token.split_at(at + 1),
        None => ("", token),
    };
    let (cached, arrival) = {
        let inner = ctx.catalog.read();
        match inner
            .remote_paths
            .get(&(target.to_string(), typed_dir.to_string()))
        {
            Some((entries, at)) => (Some(entries.clone()), Some(*at)),
            None => (None, None),
        }
    };
    if arrival.is_none_or(|at| at.elapsed() >= REVALIDATE) {
        ctx.catalog
            .write()
            .wanted
            .insert(target.to_string(), typed_dir.to_string());
    }
    let Some(entries) = cached else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|candidate| {
            let Some(name) = candidate.insert.get(typed_dir.len()..) else {
                return false;
            };
            let name = name.trim_end_matches('/');
            // A dotfile stays out of the way until asked for, as it does locally.
            if name.starts_with('.') && !prefix.starts_with('.') {
                return false;
            }
            name.starts_with(prefix)
        })
        .collect()
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
