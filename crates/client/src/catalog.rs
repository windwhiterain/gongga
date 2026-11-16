//! The completion source the TUI asks, and nowhere else.
//!
//! See `docs/notes/cli-catalog.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::commands::installed_skills;
use crate::complete::{Completer, Item, Offered, token_bounds};
use crate::json_place::{self, Read};
use crate::leaf::Leaf;
use protocol::{Args, CommandSpec, CompletionSource, ModelInfo, Reference, SessionInfo};
use serde_json::Value;

/// A catalog that can be swapped while the TUI holds a reference to it.
#[derive(Clone, Default)]
pub struct Catalog(Arc<RwLock<Inner>>);

struct Inner {
    /// Every command this client knows: the greeting, or what a front-end seeded.
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
    /// The sticky argument names the front-end holds, for the source it names but
    /// cannot fill in.
    sticky_names: Vec<String>,
    /// The models a request could run on here, and what each of them takes.
    models: Vec<ModelInfo>,
    /// The model this server runs a session on when nothing names one.
    default_model: Option<String>,
    /// The skills one directory offers, and what they were read from.
    skills: Option<Skills>,
    /// Directories a remote answered with, keyed by remote and directory, and
    /// when each answer arrived.
    remote_paths: HashMap<(String, String), (Vec<Item>, std::time::Instant)>,
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
#[derive(Clone)]
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
            default_model: None,
            skills: None,
            remote_paths: HashMap::new(),
            remotes: HashSet::new(),
            wanted: HashMap::new(),
            inflight: HashMap::new(),
            paths_version: 0,
        }
    }
}

impl Inner {
    /// The command this line names.
    ///
    /// # Invariant
    ///
    /// What was published, and nothing of this layer's own: a command a front-end
    /// runs itself is seeded through [`Catalog::set_commands`] like any other, so
    /// there is one list and one reader.
    fn find(&self, name: &str) -> Option<&CommandSpec> {
        self.published.iter().find(|c| c.name == name)
    }

    /// Every command this client completes.
    fn all(&self) -> Vec<CommandSpec> {
        self.published.clone()
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
        let named: Vec<Item> = entries
            .into_iter()
            .map(|(name, is_dir)| {
                if is_dir {
                    // Descending is what a directory is for, so no space follows.
                    Item::path(format!("{typed}{name}/"), "dir")
                } else {
                    Item::word(format!("{typed}{name}"), "file")
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

    /// Everything a completion reads, for a line whose words are already split.
    fn snapshot(&self, words: &[String]) -> Snapshot {
        let (all, sessions, sticky, models, default_model, message_session) = {
            let inner = self.read();
            (
                inner.all(),
                inner.sessions.clone(),
                inner.sticky_names.clone(),
                inner.models.clone(),
                inner.default_model.clone(),
                inner.message_session.clone(),
            )
        };
        Snapshot {
            all,
            sessions,
            sticky,
            models,
            default_model,
            message_session,
            place: self.place(words),
        }
    }

    /// The arguments of this line whose value nothing would accept.
    ///
    /// # Invariant
    ///
    /// Only an argument that publishes `accepts` can be refused here: every other
    /// source says what is worth offering rather than what is allowed, and calling
    /// a suggestion a requirement is what the `accepts` field exists to stop.
    /// See `docs/notes/cli-catalog.md`.
    pub fn refused_values(&self, line: &str, args: &Args) -> Vec<(String, String)> {
        let words: Vec<String> = words_of_line(line);
        let snapshot = self.snapshot(&words);
        let ctx = snapshot.ctx(self);
        let Some(spec) = words
            .first()
            .map(|c| c.trim_start_matches('/'))
            .and_then(|c| ctx.all.iter().find(|s| s.name == c))
        else {
            return Vec::new();
        };
        let model = model_named(&ctx, &words);
        spec.args
            .iter()
            .filter_map(|a| {
                let accepts = a.accepts.as_ref()?;
                let value = args.word(&a.name)?;
                match accepts.takes(model.as_deref(), value) {
                    Some(false) => Some((a.name.clone(), value.to_string())),
                    _ => None,
                }
            })
            .collect()
    }

    /// The sticky arguments that exist, for the source the server names.
    pub fn set_sticky_names(&self, names: &[String]) {
        self.write().sticky_names = names.to_vec();
    }

    /// The models a request could run on here, as the greeting published them.
    pub fn set_models(&self, models: Vec<ModelInfo>) {
        self.write().models = models;
    }

    /// The model this server falls back to, for a line that names none and
    /// belongs to no session yet.
    pub fn set_default_model(&self, model: &str) {
        self.write().default_model = Some(model.to_string());
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
        let session = session_on_line(
            &inner.published,
            inner.message_session.as_deref(),
            &inner.sessions,
            words,
        );
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
        // A peer's checkout is not here: the home's and the machine's are -- the
        // same rows `@skill(...)` expands against.
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
        self.read().all()
    }

    pub fn command(&self, name: &str) -> Option<CommandSpec> {
        self.read().find(name).cloned()
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
    /// The models a request could run on here, and the server's own default.
    models: &'a [ModelInfo],
    default_model: Option<&'a str>,
    /// The session a bare message goes to, which a value completes against when
    /// the line names none.
    message_session: Option<&'a str>,
    /// Where this line reads: its session's directory, and that machine.
    place: Place,
    all: &'a [CommandSpec],
    catalog: &'a Catalog,
}

/// Every source a completion reads, taken under the lock and held together.
struct Snapshot {
    all: Vec<CommandSpec>,
    sessions: Vec<SessionInfo>,
    sticky: Vec<String>,
    models: Vec<ModelInfo>,
    default_model: Option<String>,
    message_session: Option<String>,
    place: Place,
}

impl Snapshot {
    fn ctx<'a>(&'a self, catalog: &'a Catalog) -> Ctx<'a> {
        Ctx {
            sessions: &self.sessions,
            sticky: &self.sticky,
            models: &self.models,
            default_model: self.default_model.as_deref(),
            message_session: self.message_session.as_deref(),
            place: self.place.clone(),
            all: &self.all,
            catalog,
        }
    }
}

impl Completer for Catalog {
    fn wants(&self) -> bool {
        !self.read().wanted.is_empty()
    }

    fn complete(&self, line: &str, cursor: usize) -> Offered {
        let (_, _, token) = token_bounds(line, cursor);
        // `/` starts a command; a message completes only its `@` references.
        let brief = !line.trim_start().starts_with('/');
        // A message is prose, and `--session` inside prose is a word of it.
        let words: Vec<String> = if brief {
            Vec::new()
        } else {
            words_of_line(line)
        };
        let snapshot = self.snapshot(&words);
        let ctx = snapshot.ctx(self);
        if brief {
            return reference(&ctx, line, cursor, &token);
        }
        json_complete(&ctx, line, cursor, &words)
    }
}

/// Completes a message's `@image(...)`, `@skill(...)` and `@file(...)` references.
///
/// # Invariant
///
/// Two steps that never overlap: the prefix, then the value -- a path for a picture
/// or a file, a skill's name for a skill. What the popup inserts is what the parser
/// reads: `@file(` opens the group and `path)` closes it. See
/// `docs/notes/client-catalog.md`.
fn reference(ctx: &Ctx, line: &str, cursor: usize, token: &str) -> Offered {
    let (start, _, _) = token_bounds(line, cursor);
    if let Some((at, kind, typed)) = opening(token) {
        // `@file(pa`: the value is what gets replaced, from just past the `(`.
        return Offered {
            start: Some(start + at + kind.prefix().len()),
            items: closed(value(kind, typed, ctx)),
            ..Offered::default()
        };
    }
    // `@file( pa`: the value is its own word. A prefix that already carries one is
    // not: that word is prose.
    let previous = line[..start.min(line.len())].split_whitespace().next_back();
    if let Some((_, kind, "")) = previous.and_then(opening) {
        return Offered {
            items: closed(value(kind, token, ctx)),
            ..Offered::default()
        };
    }
    // `@`, `@fi`, `看下@fi`: the three prefixes themselves, and nothing else.
    let Some(at) = token.rfind('@') else {
        return Offered::default();
    };
    let bare = &token[at + 1..];
    Offered {
        start: Some(start + at),
        items: Reference::ALL
            .into_iter()
            .filter(|kind| kind.name().contains(bare))
            .map(prefix_candidate)
            .collect(),
        ..Offered::default()
    }
}

/// What a reference is for, said in the popup's right-hand column.
fn reference_detail(kind: Reference) -> &'static str {
    match kind {
        Reference::Image => "attach a picture",
        Reference::Skill => "fetch a skill's metadata",
        Reference::File => "mention a file",
    }
}

/// The prefix as a candidate: `@file(`, and no space after it, because the value
/// stands inside the group it opens.
fn prefix_candidate(kind: Reference) -> Item {
    Item::path(kind.prefix(), reference_detail(kind))
}

/// Candidates that close the group they are written in.
///
/// # Invariant
///
/// A value is inserted as `path)`, because a group the operator has to close by hand
/// is a reference the parser reads as prose until they do. A directory keeps its `/`
/// and its open group: Tab on it descends, so the `)` comes when a name is chosen.
fn closed(items: Vec<Item>) -> Vec<Item> {
    items
        .into_iter()
        .map(|item| match item.label.ends_with('/') {
            true => item,
            false => Item::word(format!("{})", item.label), item.detail.clone()),
        })
        .collect()
}

/// The reference `word` carries: its `@`'s offset, kind, and value so far.
fn opening(word: &str) -> Option<(usize, Reference, &str)> {
    let at = word.rfind('@')?;
    let rest = &word[at..];
    let kind = Reference::of_prefix(rest)?;
    Some((at, kind, &rest[kind.prefix().len()..]))
}

/// Completes one reference's value.
fn value(kind: Reference, typed: &str, ctx: &Ctx) -> Vec<Item> {
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

/// One value's completion, from what the catalog publishes about it.
///
/// # Invariant
///
/// An argument and a field of a structured value answer the one question, so
/// they read one list: the model is a parameter because the caller knows which
/// one the value belongs to, and a field's model is the one the object being
/// typed names. See `docs/notes/client-complete.md`.
fn published(
    completions: &CompletionSource,
    accepts: Option<&protocol::Accepts>,
    label: &str,
    token: &str,
    ctx: &Ctx,
    words: &[String],
    model: Option<&str>,
    depth: u8,
) -> Vec<Item> {
    if let Some(accepts) = accepts {
        let values = accepted_values(accepts, model);
        return pick(values.iter().map(String::as_str), token, label);
    }
    match completions {
        CompletionSource::None => Vec::new(),
        CompletionSource::Sessions => pick(
            ctx.sessions.iter().map(|s| s.name.as_str()),
            token,
            "session",
        ),
        CompletionSource::Static(values) => pick(values.iter().map(String::as_str), token, "value"),
        CompletionSource::Arguments => {
            // The types the catalog declares: the only sticky entries there are.
            let kinds = crate::commands::sticky_kinds(ctx.all);
            pick(kinds.iter().map(String::as_str), token, "type")
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
        // The machines this server knows, as the catalog's own `--remote` lists them.
        CompletionSource::Remotes => {
            let catalog = ctx.catalog.read();
            pick(catalog.remotes.iter().map(String::as_str), token, "remote")
        }
        CompletionSource::Skills => pick(
            ctx.catalog
                .skill_names(&ctx.place)
                .iter()
                .map(String::as_str),
            token,
            "skill",
        ),
        // The type the first word names; its value completes like that
        // type's argument. See `docs/notes/cli-catalog.md`.
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
                        && (a.accepts.is_some()
                            || !matches!(
                                a.completions,
                                CompletionSource::None | CompletionSource::OfNamedArgument
                            ))
                })
                .collect();
            // The required spelling of a name is its remote: that is what a
            // mirrored value completes against.
            match shaped
                .iter()
                .find(|a| a.required)
                .or_else(|| shaped.first())
            {
                Some(arg) => published(
                    &arg.completions,
                    arg.accepts.as_ref(),
                    arg.value_hint.as_deref().unwrap_or("value"),
                    token,
                    ctx,
                    words,
                    model,
                    depth + 1,
                ),
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
fn session_on_line<'a>(
    all: &[CommandSpec],
    message_session: Option<&str>,
    sessions: &'a [SessionInfo],
    words: &[String],
) -> Option<&'a SessionInfo> {
    let name = session_named(all, message_session, words)?;
    sessions.iter().find(|s| s.name == name)
}

/// The session a line acts on: the value of its kind-`session` argument.
/// See `docs/notes/cli-catalog.md`.
fn session_named(
    all: &[CommandSpec],
    message_session: Option<&str>,
    words: &[String],
) -> Option<String> {
    let spec = words
        .first()
        .map(|c| c.trim_start_matches('/'))
        .and_then(|c| all.iter().find(|s| s.name == c))?;
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
        .and_then(|args| args.word(&arg.name).map(str::to_string))
        .or_else(|| flag_value(words, &arg.name).map(str::to_string))
        .or_else(|| message_session.map(str::to_string))
}

/// The model a line runs on: the one it names, else the one the session it names
/// already has, else the server's own.
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
                .and_then(|args| args.word(&arg.name).map(str::to_string))
                .or_else(|| flag_value(words, &arg.name).map(str::to_string))
        });
    named
        .or_else(|| {
            session_on_line(ctx.all, ctx.message_session, ctx.sessions, words)
                .and_then(|session| session.model.clone())
        })
        .or_else(|| ctx.default_model.map(str::to_string))
}

/// The values an `accepts` publishes for a line that runs on `model`.
///
/// # Invariant
///
/// A model the table does not list accepts anything, so the union is what its
/// completion offers: withholding the list would be this client's opinion. A line
/// that names no model at all gets the same union, for the same reason.
fn accepted_values(accepts: &protocol::Accepts, model: Option<&str>) -> Vec<String> {
    match accepts {
        protocol::Accepts::Same(values) => values.clone(),
        protocol::Accepts::PerModel(table) => match model.and_then(|model| table.get(model)) {
            Some(values) => values.clone(),
            None => accepts.union(),
        },
    }
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
fn remote_paths(ctx: &Ctx, remote: &str, token: &str) -> Vec<Item> {
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
        .filter(|item| {
            let Some(name) = item.label.get(typed_dir.len()..) else {
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
fn pick<'a>(values: impl Iterator<Item = &'a str>, token: &str, detail: &str) -> Vec<Item> {
    let mut got: Vec<Item> = values
        .filter(|v| v.contains(token))
        .map(|v| Item::word(v, detail))
        .collect();
    // Stable sort, so the catalog's own order decides between equals.
    got.sort_by_key(|c| !c.label.starts_with(token));
    got
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
fn paths(base: &Path, token: &str) -> Vec<Item> {
    let (typed_dir, prefix) = split_path_token(token);
    let Ok(entries) = std::fs::read_dir(directory(base, &typed_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<Item> = entries
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
                Some(Item::path(format!("{typed_dir}{name}/"), "dir"))
            } else {
                Some(Item::word(format!("{typed_dir}{name}"), "file"))
            }
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
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

/// The line's words as the shell splits them, with a quote still open closed
/// first.
///
/// # Invariant
///
/// A value being typed is unfinished by definition -- that is what the popup is
/// for -- and an unclosed quote makes the whole line unsplittable. What the rest
/// of the completion needs from the words is which command, session and machine
/// the line names, and those are behind the quote, not inside it.
fn words_of(line: &str) -> Vec<String> {
    if let Ok(words) = shell_words::split(line) {
        return words;
    }
    let Some(quote) = open_quote(line) else {
        return Vec::new();
    };
    let mut closed = line.to_string();
    closed.push(quote);
    shell_words::split(&closed).unwrap_or_default()
}

/// The quote the line leaves open, if any.
///
/// # Invariant
///
/// The shell's own two kinds, and an escape read the way it reads one: a value
/// still being typed is not a licence to guess which quote was meant.
fn open_quote(line: &str) -> Option<char> {
    let mut open = None;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match (open, c) {
            (None, '\\') | (Some('"'), '\\') => escaped = true,
            (None, '\'') => open = Some('\''),
            (None, '"') => open = Some('"'),
            (Some('\''), '\'') | (Some('"'), '"') => open = None,
            _ => {}
        }
    }
    open
}

/// Whether a sibling follows the member being filled: another member of this
/// object is already written on the line.
///
/// # Invariant
///
/// Asked of the line, not of the schema: a field the object publishes but nobody
/// wrote is one a sticky entry fills at submission, and a comma before the close
/// would still be a trailing one the server refuses.
/// Whether a member written to end at `from` owes the comma that ends it.
///
/// # Invariant
///
/// The standard's separator rule: a comma stands before what follows, never
/// before a closer and never at the end of the line. The line decides, not the
/// schema, so what a completion writes is JSON the server can read.
fn separator(text: &str, from: usize) -> bool {
    let rest = text.get(from.min(text.len())..).unwrap_or("");
    match rest.trim_start().chars().next() {
        None | Some(',') | Some('}') | Some(']') => false,
        _ => true,
    }
}

/// The object schema at a path of field names, or `None` when the path leaves
/// the shape the catalog published.
fn schema_at<'a>(schema: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(schema, |at, name| {
        at.get("properties")
            .and_then(|properties| properties.get(name))
    })
}

/// One object's field names, as the popup offers them.
///
/// # Invariant
///
/// A name is written whole -- the colon that ends it, the brace or the quotes its
/// value is held in, the comma it owes, and every brace the object still owes --
/// and it states where the caret goes, so nothing else has to. A field the
/// object already states is not offered.
fn keys(
    object: &Value,
    fragment: &str,
    range: (usize, usize),
    written: &[(String, Read)],
    closers: usize,
    comma: bool,
) -> Vec<Item> {
    let Some(properties) = protocol::argschema::properties(object) else {
        return Vec::new();
    };
    let required = protocol::argschema::required(object);
    let mut fields: Vec<(&String, &Value)> = properties
        .iter()
        .filter(|(name, _)| name.contains(fragment))
        .filter(|(name, _)| !written.iter().any(|(held, _)| held == *name))
        .collect();
    // What the command insists on first, then a prefix match; stable, so the
    // document's own order decides between equals.
    fields.sort_by_key(|(name, _)| {
        (
            !required.contains(&name.as_str()),
            !name.starts_with(fragment),
        )
    });
    fields
        .into_iter()
        .map(|(name, property)| {
            let label = format!("\"{name}\":");
            let mut new_text = match property.get("type").and_then(Value::as_str) {
                Some("object") => format!("{label}{{$0}}"),
                Some("string") => format!("{label}\"$0\""),
                _ => format!("{label}$0"),
            };
            if comma {
                new_text.push(',');
            }
            new_text.push_str(&"}".repeat(closers));
            Item::at(
                range,
                new_text,
                true,
                label,
                protocol::argschema::description(property),
            )
        })
        .collect()
}

/// The values one leaf field of a structured object offers.
///
/// # Invariant
///
/// A model is read from the object being typed before the line is asked: `accepts`
/// is keyed by model, and another model's levels are a list the server refuses.
/// What a value is written as is its leaf's type's decision (`Leaf`), so one list
/// of texts offers `"70%"` and `true` without spelling either.
/// The values one leaf field of a structured object offers.
///
/// # Invariant
///
/// A model is read from the object being typed before the line is asked: `accepts`
/// is keyed by model, and another model's levels are a list the server refuses.
/// What a value is written as is its leaf's type's decision (`Leaf`), so one list
/// of texts offers `"70%"` and `true` without spelling either.
fn values(
    property: &Value,
    place: &json_place::Place,
    ctx: &Ctx,
    words: &[String],
    range: (usize, usize),
    closers: usize,
    comma: bool,
) -> Vec<Item> {
    let model = named_model(&place.fields).or_else(|| model_named(ctx, words));
    let hint = protocol::argschema::hint(property);
    let leaf = Leaf::from_schema(property);
    // A closed set is the standard's `enum`, and nothing else has to be asked.
    if let Some(values) = property.get("enum").and_then(Value::as_array) {
        let chosen: Vec<&str> = values
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| value.contains(&place.fragment))
            .collect();
        if !chosen.is_empty() {
            return chosen
                .into_iter()
                .filter_map(|value| {
                    let text = match protocol::argschema::model_ref(property) {
                        true => model_value(value)?,
                        false => leaf.token(value),
                    };
                    Some(member(text, &hint, range, closers, comma))
                })
                .collect();
        }
    }
    let offered = published(
        &protocol::argschema::source(property).unwrap_or_default(),
        protocol::argschema::accepts(property).as_ref(),
        &hint,
        &place.fragment,
        ctx,
        words,
        model.as_deref(),
        0,
    );
    if offered.is_empty() {
        return owed(property, leaf, place, ctx, range, closers, comma);
    }
    offered
        .into_iter()
        .filter_map(|item| {
            let value = match protocol::argschema::model_ref(property) {
                true => model_value(&item.label)?,
                false => leaf.token(&item.label),
            };
            Some(member(value, &item.detail, range, closers, comma))
        })
        .collect()
}

/// A member's value, as the item that writes it.
fn member(value: String, detail: &str, range: (usize, usize), closers: usize, comma: bool) -> Item {
    let mut new_text = format!("{value}$0");
    if comma {
        new_text.push(',');
    }
    new_text.push_str(&"}".repeat(closers));
    Item::at(range, new_text, true, value, detail.to_string())
}

/// What a leaf owes when the catalog publishes nothing for it: a string's quotes,
/// a switch's two readings.
///
/// # Invariant
///
/// A string candidate is the operator's own fragment, closed -- never a value. It is
/// offered only where there is nothing to choose from, and not while a listing is on
/// its way, whose answer it would replace. See `docs/notes/client-complete.md`.
/// What a leaf owes when the catalog publishes nothing for it: a string's quotes,
/// a switch's two readings.
///
/// # Invariant
///
/// A string candidate is the operator's own fragment, closed -- never a value. It is
/// offered only where there is nothing to choose from, and not while a listing is on
/// its way, whose answer it would replace. See `docs/notes/client-complete.md`.
fn owed(
    property: &Value,
    leaf: Leaf,
    place: &json_place::Place,
    ctx: &Ctx,
    range: (usize, usize),
    closers: usize,
    comma: bool,
) -> Vec<Item> {
    if ctx.catalog.wants() {
        return Vec::new();
    }
    let hint = protocol::argschema::hint(property);
    let texts: Vec<(String, bool)> = match leaf {
        Leaf::Switch => vec![("true".to_string(), false), ("false".to_string(), false)],
        // Both quotes stand and nothing is inside them: the token is finished.
        Leaf::Text if place.quoted => return Vec::new(),
        // An empty string leaves the caret between its quotes;
        // a typed one ends where the operator stopped.
        Leaf::Text if place.fragment.is_empty() => vec![(String::new(), true)],
        Leaf::Text => vec![(leaf.token(&place.fragment), false)],
        Leaf::Other => return Vec::new(),
    };
    texts
        .into_iter()
        .map(|(text, between_quotes)| {
            let label = match between_quotes {
                true => "\"\"".to_string(),
                false => text.clone(),
            };
            let mut new_text = match between_quotes {
                true => "\"$0\"".to_string(),
                false => format!("{text}$0"),
            };
            if comma {
                new_text.push(',');
            }
            new_text.push_str(&"}".repeat(closers));
            Item::at(range, new_text, true, label, hint.to_string())
        })
        .collect()
}

/// The model a partly typed config names, as the roster spells it.
fn named_model(fields: &[(String, Read)]) -> Option<String> {
    fields
        .iter()
        .find(|(name, _)| name == "model")
        .and_then(|(_, read)| spelled(read))
}

/// What a model field reads as: the roster's `<provider>/<model>`, or the two
/// halves SPEC gives it.
fn spelled(read: &Read) -> Option<String> {
    let half = |name: &str| match fields_of(read).iter().find(|(field, _)| field == name) {
        Some((_, Read::Text(text))) if !text.is_empty() => Some(text.clone()),
        _ => None,
    };
    match read {
        Read::Text(text) => (!text.is_empty()).then(|| text.clone()),
        Read::Object(_) => match (half("provider"), half("model")) {
            (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
            (None, Some(model)) => Some(model),
            _ => None,
        },
    }
}

/// The fields of a read object; empty for a leaf.
fn fields_of(read: &Read) -> &[(String, Read)] {
    match read {
        Read::Object(fields) => fields,
        Read::Text(_) => &[],
    }
}

/// A model as the object SPEC spells `Model := {provider, model}`.
fn model_value(spelled: &str) -> Option<String> {
    if spelled.is_empty() {
        return None;
    }
    let value = match spelled.split_once('/') {
        Some((provider, model)) => serde_json::json!({ "provider": provider, "model": model }),
        None => serde_json::json!({ "model": spelled }),
    };
    Some(value.to_string())
}

/// The JSON form's completion: a name opens `{}`, then fields and values complete
/// by the published shape.
///
/// # Invariant
///
/// Every insertion is balanced: a name carries the braces it opens, a value its
/// quotes, and each owes only the closers the text has not already written
/// (`json_place::Place`). The popup therefore never leaves a bracket or a quote
/// unmatched. See `docs/notes/client-complete.md`.
fn json_complete(ctx: &Ctx, line: &str, cursor: usize, words: &[String]) -> Offered {
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead + 1..];
    let (name, tail) = crate::json_command::split(rest);
    let name_end = lead + 1 + name.len();
    let spec = ctx.all.iter().find(|c| c.name == name);
    let after_name = &rest[name.len()..];
    let gap = after_name.len() - after_name.trim_start().len();
    if crate::json_command::is_json(&tail) {
        let brace = name_end + gap;
        if cursor >= brace {
            let Some(spec) = spec else {
                return Offered::default();
            };
            let schema = root_schema(spec);
            return object_completion(
                ctx,
                &schema,
                &name,
                &line[brace..],
                cursor - brace,
                brace,
                words,
            );
        }
        // Before the object the name is still the token being typed.
        return command_names(ctx, &name, lead, name_end);
    }
    // No object yet: completing the name, which opens the object when accepted.
    if cursor <= name_end || tail.is_empty() {
        return command_names(ctx, &name, lead, name_end);
    }
    // A flag-spelled line is not the form this completes; it still parses.
    Offered::default()
}

/// Command names, matched anywhere; accepting one opens its argument object,
/// caret inside.
fn command_names(ctx: &Ctx, fragment: &str, start: usize, end: usize) -> Offered {
    let mut got: Vec<Item> = ctx
        .all
        .iter()
        .filter(|c| c.name.contains(fragment))
        .map(|c| match c.args.is_empty() {
            true => Item::path(format!("/{}", c.name), c.description.clone()),
            // The name opens the object, and the caret lands inside it.
            false => Item::at(
                (start, end),
                format!("/{}{{$0}}", c.name),
                true,
                format!("/{}", c.name),
                c.description.clone(),
            ),
        })
        .collect();
    // A prefix match is what the operator is most likely typing; stable, so the
    // catalog's order decides between equals.
    got.sort_by_key(|c| !c.label.trim_start_matches('/').starts_with(fragment));
    Offered {
        subject: None,
        items: got,
        start: Some(start),
        end: Some(end),
    }
}

/// Inside the argument object: field names where a key is expected, leaf values
/// where one is filled.
fn object_completion(
    ctx: &Ctx,
    schema: &Value,
    cmd: &str,
    text: &str,
    at: usize,
    brace: usize,
    words: &[String],
) -> Offered {
    let Some(place) = json_place::place(text, at) else {
        return Offered::default();
    };
    let range = (brace + place.start, brace + place.end);
    let owed = place.open.saturating_sub(place.after);
    // The separator is the line's answer, not the schema's.
    let comma = separator(text, place.end);
    match place.typing {
        json_place::Typing::Key => {
            let Some(object) = schema_at(schema, &place.path) else {
                return Offered::default();
            };
            Offered {
                subject: Some(subject_name(cmd, &place.path)),
                start: Some(range.0),
                end: Some(range.1),
                items: keys(object, &place.fragment, range, &place.fields, owed, comma),
            }
        }
        json_place::Typing::Value => {
            let Some((name, parent)) = place.path.split_last() else {
                return Offered::default();
            };
            let Some(object) = schema_at(schema, parent) else {
                return Offered::default();
            };
            let Some(property) = object.get("properties").and_then(|held| held.get(name)) else {
                return Offered::default();
            };
            // An object field is entered by its name, which wrote the brace.
            if property.get("type").and_then(Value::as_str) == Some("object") {
                return Offered::default();
            }
            Offered {
                subject: Some(subject_name(cmd, &place.path)),
                start: Some(range.0),
                end: Some(range.1),
                items: values(property, &place, ctx, words, range, owed, comma),
            }
        }
    }
}

/// The command's arguments as one JSON Schema document, built by the one builder
/// the model's tool schema uses.
fn root_schema(spec: &CommandSpec) -> Value {
    protocol::argschema::of(&spec.args)
}

/// What the popup asks for: the command at the root, then the fields down to the
/// token.
fn subject_name(cmd: &str, path: &[String]) -> String {
    if path.is_empty() {
        cmd.to_string()
    } else {
        path.join(".")
    }
}

/// The line's words for the context readers: the JSON form projects its fields
/// to the flag spelling they bind as.
fn words_of_line(line: &str) -> Vec<String> {
    let Some(rest) = line.trim_start().strip_prefix('/') else {
        return Vec::new();
    };
    let (name, tail) = crate::json_command::split(rest);
    if crate::json_command::is_json(&tail) {
        return project_json(&name, &tail);
    }
    words_of(line)
}

/// A JSON object's fields as the flag tokens the context readers parse; a
/// half-typed one projects its name alone.
fn project_json(name: &str, tail: &str) -> Vec<String> {
    let mut words = vec![name.to_string()];
    if let Ok(map) = crate::json_command::parse_object(tail) {
        for (field, value) in map {
            words.push(format!("--{field}"));
            words.push(crate::json_command::word(&value));
        }
    }
    words
}
