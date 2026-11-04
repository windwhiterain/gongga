//! The completion source the TUI asks, and nowhere else.
//!
//! See `docs/notes/cli-catalog.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::commands::installed_skills;
use crate::complete::{Candidate, Completer, token_bounds};
use protocol::{
    ArgSpec, CommandSpec, CommandTarget, CompletionSource, ModelInfo, Reference, SessionInfo,
};

/// A catalog that can be swapped while the TUI holds a reference to it.
#[derive(Clone, Default)]
pub struct Catalog(Arc<RwLock<Inner>>);

struct Inner {
    /// Everything the server published, as published: this client keeps no
    /// command list of its own.
    published: Vec<CommandSpec>,
    /// The live sessions, as the server published them: the names a value
    /// completes against, and where each one works.
    ///
    /// # Invariant
    ///
    /// A value that belongs to a session -- a skill's name, a relative path -- is
    /// read from *that* session's directory, never from this process's: the server
    /// resolves it in the session, and a window launched elsewhere would otherwise
    /// offer a name that cannot be opened.
    sessions: Vec<SessionInfo>,
    /// The session a bare message goes to, as the sticky arguments name it.
    message_session: Option<String>,
    /// The harness home, whose `skills/` is the second root.
    home: PathBuf,
    /// Where a local session starts: the server's own directory, as it published it.
    ///
    /// # Invariant
    ///
    /// The server's, not this process's: a value completed here is resolved there,
    /// and two directories that agree only by luck is what this file keeps apart.
    server_cwd: PathBuf,
    /// The client's sticky argument names, for the sources the server names
    /// but cannot fill in.
    sticky_names: Vec<String>,
    /// The models a request could run on here, and what each of them takes.
    models: Vec<ModelInfo>,
    /// The sticky `model` value: the model a `--thinking` value would apply to.
    sticky_model: Option<String>,
    /// The skills one directory offers, and what they were read from.
    skills: Option<Skills>,
    /// Directories a remote answered with, keyed by remote and directory, and
    /// when each answer arrived.
    remote_paths: HashMap<(String, String), (Vec<Candidate>, std::time::Instant)>,
    /// The names a config holds, for reading `--remote` off the line. Taken from
    /// the published catalog rather than kept here.
    remotes: HashSet<String>,
    /// The directory each remote's line last asked for and has no answer for.
    ///
    /// # Invariant
    ///
    /// One entry per remote, never one per directory: the line is completed on
    /// every keystroke, so a queue keyed by directory asks for a listing per letter
    /// typed, answers them in order, and is always behind the cursor.
    wanted: HashMap<String, String>,
    /// The directory each remote is being asked for right now, so a second
    /// question never queues behind the first.
    inflight: HashMap<String, String>,
    /// Bumped whenever `remote_paths` changes, so a front-end repaints the popup.
    paths_version: u64,
}

/// Where one input line's relative values are read, and how.
struct Place {
    /// The directory: the line's session's own, or the one the server runs in.
    dir: PathBuf,
    /// The machine that directory is on, when it is not this one.
    peer: Option<String>,
}

/// What a cached skill list was read from.
///
/// # Invariant
///
/// The two things the answer depends on and nothing else, so a session that moves
/// -- or a line that addresses another one -- is read again.
#[derive(PartialEq, Eq)]
struct SkillKey {
    project: Option<PathBuf>,
    home: PathBuf,
}

/// A skill list, and what it was read from.
struct Skills {
    key: SkillKey,
    names: Vec<String>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            published: Vec::new(),
            sessions: Vec::new(),
            message_session: None,
            home: PathBuf::new(),
            server_cwd: std::env::current_dir().unwrap_or_default(),
            sticky_names: Vec::new(),
            models: Vec::new(),
            sticky_model: None,
            skills: None,
            remote_paths: HashMap::new(),
            remotes: HashSet::new(),
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
        // An argument named `remote` publishes the names a config holds: the
        // client reads them off the catalog.
        let names: HashSet<String> = commands
            .iter()
            .flat_map(|command| command.args.iter())
            .filter(|arg| arg.name == "remote")
            .flat_map(|arg| match &arg.completions {
                CompletionSource::Static(values) => values.clone(),
                _ => Vec::new(),
            })
            .collect();
        self.write().remotes = names;
        self.write().published = commands;
    }

    pub fn set_sessions(&self, sessions: Vec<SessionInfo>) {
        self.write().sessions = sessions;
    }

    /// The session a bare message goes to, as the sticky arguments name it.
    pub fn set_message_session(&self, name: Option<&str>) {
        self.write().message_session = name.map(str::to_string);
    }

    /// The home whose `skills/` is read, and the directory the server itself runs in.
    pub fn set_roots(&self, home: &Path, server_cwd: &Path) {
        let mut inner = self.write();
        inner.home = home.to_path_buf();
        if !server_cwd.as_os_str().is_empty() {
            inner.server_cwd = server_cwd.to_path_buf();
        }
    }

    /// The listings a completion wants, as `(remote, directory)`; taking them
    /// is what stops a repeat ask.
    pub fn take_wanted(&self) -> Vec<(String, String)> {
        let mut inner = self.write();
        let asked = std::mem::take(&mut inner.wanted);
        let mut sending = Vec::new();
        for (remote, directory) in asked {
            // Already asked: what is missing is the answer, not the question.
            if inner.inflight.get(&remote) == Some(&directory) {
                continue;
            }
            inner.inflight.insert(remote.clone(), directory.clone());
            sending.push((remote, directory));
        }
        sending
    }

    /// Records what a remote answered, so the next completion offers it.
    pub fn fill_paths(&self, remote: &str, directory: &str, entries: Vec<(String, bool)>) {
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
        // This remote may be asked again, and the line says what it now wants.
        if inner.inflight.get(remote).map(String::as_str) == Some(directory) {
            inner.inflight.remove(remote);
        }
        inner.remote_paths.insert(
            (remote.to_string(), directory.to_string()),
            (named, std::time::Instant::now()),
        );
        inner.paths_version += 1;
    }

    /// Records that a remote did not answer one listing.
    ///
    /// # Invariant
    ///
    /// The question is no longer in flight: a refused or unanswered listing must
    /// not be remembered as one still being answered, or the same directory is
    /// never asked for again and its popup stays empty for good.
    pub fn fail_paths(&self, remote: &str) {
        self.write().inflight.remove(remote);
    }

    /// How many listings have arrived; a front-end repaints when this changes.
    pub fn paths_version(&self) -> u64 {
        self.read().paths_version
    }

    /// The sticky arguments that exist, for the source the server names.
    pub fn set_sticky_names(&self, names: &[String]) {
        self.write().sticky_names = names.to_vec();
    }

    /// The models a request could run on here, as the greeting published them.
    pub fn set_models(&self, models: Vec<ModelInfo>) {
        self.write().models = models;
    }

    /// The model the sticky arguments name: what a tuning value applies to.
    pub fn set_sticky_model(&self, model: Option<&str>) {
        self.write().sticky_model = model.map(str::to_string);
    }

    /// Where one input line's values are read, and which machine they are on.
    ///
    /// # Invariant
    ///
    /// Three answers, in this order: the machine `--remote` names, then the machine
    /// the line's session runs on, then this one. The directory is the session's own
    /// when the line has one, and the server's otherwise. See
    /// `docs/notes/cli-catalog.md`.
    fn place(&self, words: &[String]) -> Place {
        let inner = self.read();
        let session = session_on_line(&inner, words);
        let peer = named_machine(&inner, words).or_else(|| session.and_then(|s| s.remote.clone()));
        let dir = session
            .map(|s| PathBuf::from(&s.cwd))
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| inner.server_cwd.clone());
        Place { dir, peer }
    }

    /// The skills a line can open, read where its session reads them.
    ///
    /// # Invariant
    ///
    /// Keyed by what the answer depends on, so a session that moves is read again
    /// and no call site has to remember to ask. See `docs/notes/cli-catalog.md`.
    fn skill_names(&self, place: &Place) -> Vec<String> {
        // A peer's checkout is not here: the home's and the machine's are, and
        // the server's own `@skill:` expansion reads those.
        let key = SkillKey {
            project: place.peer.is_none().then(|| place.dir.clone()),
            home: self.read().home.clone(),
        };
        let cached = {
            let inner = self.read();
            inner
                .skills
                .as_ref()
                .filter(|read| read.key == key)
                .map(|read| read.names.clone())
        };
        if let Some(names) = cached {
            return names;
        }
        let names = installed_skills(&key.home, key.project.as_deref());
        self.write().skills = Some(Skills {
            key,
            names: names.clone(),
        });
        names
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
    pub fn fills_sticky(&self, kind: &str) -> Option<bool> {
        let published = self.read().published.clone();
        if published.is_empty() {
            return None;
        }
        // The catalog decides which entries exist: a type some published
        // argument declares, never a name.
        Some(
            published
                .iter()
                .flat_map(|command| command.args.iter())
                .any(|a| a.kind.as_deref() == Some(kind)),
        )
    }

    /// The types a sticky entry could ever name, sorted, for a refusal to name.
    pub fn sticky_names(&self) -> Vec<String> {
        let published = self.read().published.clone();
        let mut names: Vec<String> = published
            .iter()
            .flat_map(|command| command.args.iter())
            .filter_map(|a| a.kind.clone())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Whether the caller runs this one itself. Answered from the published
    /// remote, never from a list here.
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
/// except the listing cache.
struct Ctx<'a> {
    sessions: &'a [SessionInfo],
    sticky: &'a [String],
    /// The models a request could run on here, and the sticky one this line runs on.
    models: &'a [ModelInfo],
    sticky_model: Option<&'a str>,
    /// Where this line reads: its session's directory, and that machine.
    place: Place,
    all: &'a [CommandSpec],
    catalog: &'a Catalog,
}

impl Completer for Catalog {
    fn wants(&self) -> bool {
        !self.read().wanted.is_empty()
    }

    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
        let (_, _, token) = token_bounds(line, cursor);
        let (all, sessions, sticky, models, sticky_model) = {
            let inner = self.read();
            (
                inner.published.clone(),
                inner.sessions.clone(),
                inner.sticky_names.clone(),
                inner.models.clone(),
                inner.sticky_model.clone(),
            )
        };
        // `/` starts a command; a message completes only its `@` references.
        // See `docs/notes/cli-catalog.md`.
        let brief = !line.trim_start().starts_with('/');
        // A message is prose, and `--session` inside prose is a word of it.
        let words: Vec<String> = if brief {
            Vec::new()
        } else {
            shell_words::split(line).unwrap_or_default()
        };
        let ctx = Ctx {
            sessions: &sessions,
            sticky: &sticky,
            models: &models,
            sticky_model: sticky_model.as_deref(),
            place: self.place(&words),
            all: &all,
            catalog: self,
        };
        if brief {
            return reference(&ctx, line, cursor, &token);
        }
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

        let Some(spec) = cmd.and_then(|c| all.iter().find(|s| s.name == c)) else {
            return Vec::new();
        };
        // A word completes the flag's value or the next orderable slot -- a
        // positional or a required argument, whichever binds next.
        // See `docs/notes/cli-catalog.md`.
        let flag_index = if token.is_empty() {
            position.checked_sub(1)
        } else {
            position.checked_sub(2)
        };
        if let Some(flag) = flag_index
            .and_then(|i| words.get(i))
            .and_then(|w| w.strip_prefix("--"))
        {
            let Some(arg) = spec.args.iter().find(|a| a.name == flag) else {
                return Vec::new();
            };
            return resolve(&arg.completions, &token, &ctx, &words, 0);
        }
        let filled = positional_tokens(&spec, &words, word_at_cursor).len();
        let flag_bound: std::collections::HashSet<&str> = words
            .iter()
            .filter_map(|w| w.strip_prefix("--"))
            .filter_map(|f| f.split('=').next())
            .collect();
        spec.args
            .iter()
            .filter(|a| (a.positional || a.required) && !flag_bound.contains(a.name.as_str()))
            .nth(filled)
            .map(|arg| resolve(&arg.completions, &token, &ctx, &words, 0))
            .unwrap_or_default()
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
        Reference::Skill => pick(
            ctx.catalog
                .skill_names(&ctx.place)
                .iter()
                .map(String::as_str),
            typed,
            "skill",
        ),
        Reference::Image | Reference::File => match &ctx.place.peer {
            Some(remote) => remote_paths(ctx, remote, typed),
            None => paths(&ctx.place.dir, typed),
        },
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
        CompletionSource::Sessions => pick(
            ctx.sessions.iter().map(|s| s.name.as_str()),
            token,
            "session",
        ),
        CompletionSource::Static(values) => pick(values.iter().map(String::as_str), token, "value"),
        CompletionSource::Arguments => {
            // The types the catalog declares: the only sticky entries there are.
            let mut kinds: Vec<&str> = ctx
                .all
                .iter()
                .flat_map(|c| c.args.iter())
                .filter_map(|a| a.kind.as_deref())
                .collect();
            kinds.sort_unstable();
            kinds.dedup();
            pick(kinds.into_iter(), token, "type")
        }
        // The line's session decides: its own directory here, a listing from the
        // peer when its shells run elsewhere.
        CompletionSource::Paths => match &ctx.place.peer {
            Some(remote) => remote_paths(ctx, remote, token),
            None => paths(&ctx.place.dir, token),
        },
        CompletionSource::StickyArguments => {
            pick(ctx.sticky.iter().map(String::as_str), token, "argument")
        }
        CompletionSource::Models => pick(
            ctx.models.iter().map(|model| model.model.as_str()),
            token,
            "model",
        ),
        CompletionSource::ModelLevels => {
            let levels = level_choices(ctx, words);
            pick(levels.iter().map(String::as_str), token, "level")
        }
        CompletionSource::Skills => pick(
            ctx.catalog
                .skill_names(&ctx.place)
                .iter()
                .map(String::as_str),
            token,
            "skill",
        ),
        // The type the line's first word names: a set_sticky whose type
        // token completes its value the way that type's argument does.
        // See `docs/notes/cli-catalog.md`.
        CompletionSource::OfNamedArgument if depth < 2 => {
            let named = first_positional(ctx, words);
            let Some(named) = named else {
                return Vec::new();
            };
            let shaped: Vec<&protocol::ArgSpec> = ctx
                .all
                .iter()
                .flat_map(|c| c.args.iter())
                .filter(|a| {
                    a.kind.as_deref() == Some(named)
                        && !matches!(
                            a.completions,
                            CompletionSource::None | CompletionSource::OfNamedArgument
                        )
                })
                .collect();
            // The required spelling of a name is its remote: that is what a
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

/// The first positional word on the line, when the command keeps one.
fn first_positional<'a>(ctx: &Ctx<'_>, words: &'a [String]) -> Option<&'a str> {
    let spec = words
        .first()
        .map(|c| c.trim_start_matches('/'))
        .and_then(|c| ctx.all.iter().find(|s| s.name == c))?;
    positional_tokens(spec, words, None).first().copied()
}

/// How long an answer stands before the same directory is asked for again.
///
/// # Invariant
///
/// Above zero, because a repaint re-asks: an answer that were always stale would
/// turn every refresh into another peer round-trip, and the refresh into a loop.
const REVALIDATE: std::time::Duration = std::time::Duration::from_secs(1);

/// The session this line acts on: the one it names, else the one a bare
/// message goes to. See `docs/notes/cli-catalog.md`.
fn session_on_line<'a>(inner: &'a Inner, words: &[String]) -> Option<&'a SessionInfo> {
    let name = session_named(inner, words)?;
    inner.sessions.iter().find(|s| s.name == name)
}

/// The session name a line carries: the value bound to its kind-`session`
/// argument, wherever the spec keeps that argument and however it is spelled.
fn session_named(inner: &Inner, words: &[String]) -> Option<String> {
    let spec = words
        .first()
        .map(|c| c.trim_start_matches('/'))
        .and_then(|c| inner.published.iter().find(|s| s.name == c))?;
    let arg = spec
        .args
        .iter()
        .find(|a| a.kind.as_deref() == Some("session"))?;
    // The one binder decides what the line says; a half-typed flag value still
    // names a session to complete for.
    let mut tokens = vec![spec.name.clone()];
    tokens.extend(words.iter().skip(1).cloned());
    crate::commands::parse_args(&tokens, Some(spec))
        .ok()
        .and_then(|args| args.get(&arg.name).cloned())
        .or_else(|| flag_value(words, &arg.name).map(str::to_string))
        .or_else(|| inner.message_session.clone())
}

/// The model a line runs on: the one it names, else the sticky one.
fn model_named(ctx: &Ctx<'_>, words: &[String]) -> Option<String> {
    let spec = words
        .first()
        .map(|c| c.trim_start_matches('/'))
        .and_then(|c| ctx.all.iter().find(|s| s.name == c))?;
    let named = spec
        .args
        .iter()
        .find(|a| a.kind.as_deref() == Some("model"))
        .and_then(|arg| {
            let mut tokens = vec![spec.name.clone()];
            tokens.extend(words.iter().skip(1).cloned());
            crate::commands::parse_args(&tokens, Some(spec))
                .ok()
                .and_then(|args| args.get(&arg.name).cloned())
                .or_else(|| flag_value(words, &arg.name).map(str::to_string))
        });
    named.or_else(|| ctx.sticky_model.map(str::to_string))
}

/// The levels the line's model takes; every level some model here takes when
/// the line names none.
///
/// # Invariant
///
/// A model the roster says nothing about accepts anything, so the union is what
/// its completion offers: withholding the list would be this client's opinion.
fn level_choices(ctx: &Ctx<'_>, words: &[String]) -> Vec<String> {
    let stated = model_named(ctx, words)
        .and_then(|model| ctx.models.iter().find(|info| info.model == model))
        .map(|info| &info.levels)
        .filter(|levels| !levels.is_empty());
    // Unstated takes whatever it is sent, so every level some model here
    // publishes is a fair guess.
    let source: Box<dyn Iterator<Item = &String>> = match stated {
        Some(levels) => Box::new(levels.iter()),
        None => Box::new(ctx.models.iter().flat_map(|info| info.levels.iter())),
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for level in source {
        if seen.insert(level.as_str()) {
            out.push(level.clone());
        }
    }
    out.push(protocol::THINKING_DEFAULT.to_string());
    out
}

/// The bare words of a command line, in order; `skip` is the cursor's
/// word. A known flag consumes the next.
fn positional_tokens<'a>(
    spec: &CommandSpec,
    words: &'a [String],
    skip: Option<usize>,
) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < words.len() {
        if Some(i) == skip {
            i += 1;
            continue;
        }
        let word = &words[i];
        if let Some(flag) = word.strip_prefix("--") {
            let name = flag.split('=').next().unwrap_or(flag);
            let known = spec.args.iter().any(|a| a.name == name);
            i += if known && !flag.contains('=') { 2 } else { 1 };
        } else {
            out.push(word.as_str());
            i += 1;
        }
    }
    out
}

/// The machine `--remote` names, when the config holds it.
fn named_machine(inner: &Inner, words: &[String]) -> Option<String> {
    let asked = flag_value(words, "remote")?;
    inner.remotes.contains(asked).then(|| asked.to_string())
}

/// Completes `--cwd` against the filesystem of the machine the line names.
///
/// # Invariant
///
/// Never waits for a peer: what has arrived is offered now, and the rest is asked
/// for, so a completion never blocks the front-end on a network.
fn remote_paths(ctx: &Ctx, remote: &str, token: &str) -> Vec<Candidate> {
    let (typed_dir, prefix) = split_path_token(token);
    let (cached, arrival) = {
        let inner = ctx.catalog.read();
        match inner
            .remote_paths
            .get(&(remote.to_string(), typed_dir.clone()))
        {
            Some((entries, at)) => (Some(entries.clone()), Some(*at)),
            None => (None, None),
        }
    };
    if arrival.is_none_or(|at| at.elapsed() >= REVALIDATE) {
        ctx.catalog
            .write()
            .wanted
            .insert(remote.to_string(), typed_dir.clone());
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
            name.starts_with(&prefix)
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

/// Lists the filesystem under `base`, keeping the directory part already typed; a
/// directory keeps its trailing `/`.
fn paths(base: &Path, token: &str) -> Vec<Candidate> {
    let (typed_dir, prefix) = split_path_token(token);
    let Ok(entries) = std::fs::read_dir(directory(base, &typed_dir)) else {
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
            if !name.starts_with(&prefix) {
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

/// The directory part of a path token and the name prefix inside it.
///
/// # Invariant
///
/// A bare drive letter names the drive root, not a name starting with that letter:
/// on Windows `C` and `C:` both mean `C:/`. Read as a prefix it listed the start
/// directory's entries and inserted one of them over the letter the operator typed,
/// which measured as `--cwd C` becoming `--cwd Contacts/`.
fn split_path_token(token: &str) -> (String, String) {
    if cfg!(windows) && !token.is_empty() && token.as_bytes()[0].is_ascii_alphabetic() {
        let bare_drive = match token.len() {
            1 => true,
            2 => token.as_bytes()[1] == b':',
            _ => false,
        };
        if bare_drive {
            return (format!("{}:/", &token[..1]), String::new());
        }
    }
    match token.rfind(['/', '\\']) {
        Some(at) => (token[..at + 1].to_string(), token[at + 1..].to_string()),
        None => (String::new(), token.to_string()),
    }
}

/// The directory `typed` names, relative to `base`, with `~` expanded.
fn directory(base: &Path, typed: &str) -> PathBuf {
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
        base.join(path)
    }
}
