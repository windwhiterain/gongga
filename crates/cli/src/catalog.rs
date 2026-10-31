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

    /// Points path completion at a directory other than the process's; for
    /// tests.
    #[cfg(test)]
    pub fn set_cwd(&self, cwd: impl Into<PathBuf>) {
        self.write().cwd = cwd.into();
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

/// One argument of a catalog entry, for tests that build a greeting by hand.
#[cfg(test)]
fn arg(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> ArgSpec {
    ArgSpec {
        name: name.into(),
        description: description.into(),
        required,
        value_hint: value_hint.map(str::to_string),
        completions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for a greeting: the server's own commands plus the client's,
    /// exactly what it publishes.
    fn catalog() -> Catalog {
        let c = Catalog::new();
        c.set_sticky_names(&["session".to_string()]);

        let mut published = protocol::client_commands();
        published.extend(vec![
            CommandSpec {
                target: CommandTarget::Server,
                name: "send_message".into(),
                description: "Send a message.".into(),
                args: vec![
                    arg("message", "The text.", true, None, CompletionSource::None),
                    arg(
                        "session",
                        "Target.",
                        false,
                        None,
                        CompletionSource::Sessions,
                    ),
                ],
            },
            CommandSpec {
                target: CommandTarget::Server,
                name: "set_session".into(),
                description: "Create or configure.".into(),
                args: vec![
                    arg("session", "Target.", true, None, CompletionSource::Sessions),
                    arg("cwd", "Directory.", false, None, CompletionSource::Paths),
                ],
            },
        ]);
        c.set_commands(published);
        c.set_sessions(vec!["main".into(), "scratch".into()]);
        c
    }

    fn values(c: &Catalog, line: &str) -> Vec<String> {
        c.complete(line, line.len())
            .into_iter()
            .map(|x| x.insert)
            .collect()
    }

    #[test]
    fn completes_command_names_from_both_catalogs() {
        let c = catalog();
        assert_eq!(values(&c, "/set_st"), vec!["/set_sticky"]);
        assert_eq!(values(&c, "/send"), vec!["/send_message"]);
    }

    #[test]
    fn completes_flag_names() {
        let c = catalog();
        assert_eq!(values(&c, "/send_message --s"), vec!["--session"]);
    }

    /// A flag already on the line is not offered again: it is filled, and the
    /// list is there to say what is still missing.
    #[test]
    fn a_filled_flag_is_not_offered_again() {
        let c = catalog();
        assert_eq!(
            values(&c, "/send_message --session main --"),
            vec!["--message"]
        );
        // The `--flag=value` spelling counts as filled too.
        assert_eq!(
            values(&c, "/send_message --session=main --"),
            vec!["--message"]
        );
        // Typing the filled flag's name again finds nothing, rather than
        // offering the one already there.
        assert!(values(&c, "/send_message --session main --s").is_empty());
        // Once everything is on the line there is nothing left to offer.
        assert!(values(&c, "/send_message --session main --message hi --").is_empty());
    }

    /// The word being completed is not "already filled", or no flag could ever
    /// be completed.
    #[test]
    fn the_flag_being_typed_is_still_a_candidate() {
        let c = catalog();
        assert_eq!(values(&c, "/send_message --sess"), vec!["--session"]);
    }

    /// A space after the command name starts its flags, not a second command
    /// choice.
    #[test]
    fn a_finished_command_does_not_reopen_the_command_list() {
        let c = catalog();
        assert!(values(&c, "/send_message ").is_empty());
    }

    #[test]
    fn completes_flag_values_from_sessions() {
        let c = catalog();
        assert_eq!(
            values(&c, "/send_message --session "),
            vec!["main", "scratch"]
        );
        assert_eq!(values(&c, "/send_message --session sc"), vec!["scratch"]);
    }

    /// SPEC: a candidate matches at any character position, not only at the front.
    #[test]
    fn candidates_match_anywhere_in_the_word() {
        let c = catalog();
        // `message` is inside `send_message` and behind it: both are offered.
        let got = values(&c, "/set_render_session --ses");
        assert_eq!(got, vec!["--session"]);
        let got = values(&c, "/set_sticky --name s");
        assert!(got.contains(&"session".to_string()), "{got:?}");
        // An exact match comes first, so Tab on a whole word changes nothing.
        let got = values(&c, "/set_render_session --session ma");
        assert_eq!(got.first().map(String::as_str), Some("main"), "{got:?}");
    }

    /// SPEC: a message's three references complete, prefix first and then value.
    #[test]
    fn a_message_completes_its_references() {
        let c = catalog();
        c.set_skill_names(&["evolve".to_string(), "heir".to_string()]);
        let got = values(&c, "@");
        assert_eq!(got, vec!["@image:", "@skill:", "@file:"]);
        assert_eq!(values(&c, "@sk"), vec!["@skill:"]);
        // A skill completes against what is installed here.
        assert_eq!(values(&c, "@skill: he"), vec!["heir"]);
        // A picture or a file completes against the filesystem.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), b"x").unwrap();
        c.set_cwd(dir.path());
        assert_eq!(values(&c, "@image: shot"), vec!["shot.png"]);
        assert_eq!(values(&c, "@file: shot"), vec!["shot.png"]);
        // Written together, the value completes the same way.
        assert_eq!(values(&c, "@image:shot"), vec!["shot.png"]);
        // A message with no reference in reach offers nothing.
        assert!(values(&c, "hello ").is_empty());
    }

    #[test]
    fn free_form_values_yield_nothing() {
        let c = catalog();
        assert!(values(&c, "/send_message --message ").is_empty());
    }

    #[test]
    fn sticky_names_complete_against_state() {
        let c = catalog();
        assert_eq!(values(&c, "/del_sticky --name "), vec!["session"]);
    }

    /// `--name` takes an argument name, so it offers the catalog's, once each,
    /// whichever command declared them.
    #[test]
    fn argument_names_complete_from_the_whole_catalog() {
        let c = catalog();
        let got = values(&c, "/set_sticky --name ");
        assert_eq!(
            got,
            vec![
                "block_size",
                "cwd",
                "ip",
                "message",
                "mode",
                "name",
                "port",
                "session",
                "value"
            ]
        );
    }

    /// A sticky value completes like the argument it fills: `session` offers
    /// sessions.
    #[test]
    fn a_sticky_value_completes_like_the_argument_it_fills() {
        let c = catalog();
        assert_eq!(
            values(&c, "/set_sticky --name session --value "),
            vec!["main", "scratch"]
        );
        assert_eq!(
            values(&c, "/set_sticky --name=session --value ma"),
            vec!["main"]
        );
    }

    /// A free-form argument stays free-form, and so does anything the catalog
    /// does not know.
    #[test]
    fn a_sticky_value_for_a_free_form_argument_offers_nothing() {
        let c = catalog();
        assert!(values(&c, "/set_sticky --name message --value ").is_empty());
        assert!(values(&c, "/set_sticky --value main").is_empty());
    }

    /// A path value lists the directory it names, and a directory is inserted
    /// without a space so the next Tab can descend.
    #[test]
    fn paths_complete_from_the_working_directory() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::create_dir(d.path().join("dup")).unwrap();
        std::fs::write(d.path().join("solo.txt"), "x").unwrap();
        std::fs::write(d.path().join(".hidden"), "x").unwrap();
        std::fs::write(d.path().join("sub").join("inner.txt"), "x").unwrap();
        let c = catalog();
        c.set_cwd(d.path());

        let line = "/set_session --session s --cwd s";
        let got = c.complete(line, line.len());
        let dir = got
            .iter()
            .find(|x| x.insert == "sub/")
            .expect("the directory");
        assert!(!dir.space, "a directory must not be followed by a space");
        let file = got
            .iter()
            .find(|x| x.insert == "solo.txt")
            .expect("the file");
        assert!(file.space, "a file ends the word");

        // Descending: the trailing `/` keeps the token inside the directory.
        assert_eq!(values(&c, "/set_session --cwd sub/"), vec!["sub/inner.txt"]);
        // Dotfiles only when asked for.
        assert!(
            values(&c, "/set_session --cwd ")
                .iter()
                .all(|v| !v.starts_with('.'))
        );
        assert_eq!(values(&c, "/set_session --cwd .h"), vec![".hidden"]);
        // A prefix that names nothing is not an error, just no candidates.
        assert!(values(&c, "/set_session --cwd nowhere/").is_empty());
    }

    /// SPEC: a command name matches anywhere, not only at the front.
    #[test]
    fn a_command_name_matches_anywhere() {
        let c = catalog();
        // `/render` is inside `set_render` and `set_render_session`.
        let got = values(&c, "/render");
        assert_eq!(got, vec!["/set_render", "/set_render_session"], "{got:?}");
        // The words that did match before still match, and sort first.
        assert_eq!(
            values(&c, "/set_st")[0],
            "/set_sticky",
            "{:?}",
            values(&c, "/set_st")
        );
        // A word inside a command's tail finds it too: `session` is not a prefix.
        let got = values(&c, "/session");
        assert!(got.contains(&"/set_session".to_string()), "{got:?}");
    }

    /// `set_render` takes one argument, and it completes to the sizes worth typing.
    #[test]
    fn the_render_command_completes_its_sizes() {
        let c = catalog();
        assert_eq!(values(&c, "/set_render --"), vec!["--block_size"]);
        assert_eq!(
            values(&c, "/set_render --block_size "),
            vec!["0", "1", "2", "3", "5"]
        );
        // Any number is a size: the list above is what is worth offering, not the
        // set that is allowed.
        assert_eq!(values(&c, "/set_render --block_size 1"), vec!["1"]);
    }

    /// `set_render_session` names a session and then one of the three modes.
    #[test]
    fn the_render_session_command_completes_sessions_and_modes() {
        let c = catalog();
        // Catalog order, which is the order the command declares them in.
        assert_eq!(
            values(&c, "/set_render_session --"),
            vec!["--session", "--mode"]
        );
        assert_eq!(
            values(&c, "/set_render_session --session "),
            vec!["main", "scratch"]
        );
        assert_eq!(
            values(&c, "/set_render_session --mode "),
            vec!["none", "line", "full"]
        );
        assert_eq!(values(&c, "/set_render_session --mode f"), vec!["full"]);
        // An argument the line already carries is not offered again.
        assert_eq!(
            values(&c, "/set_render_session --mode line --"),
            vec!["--session"]
        );
    }

    #[test]
    fn the_render_commands_run_in_the_client() {
        let c = catalog();
        assert!(c.is_client_command("set_render"));
        assert!(c.is_client_command("set_render_session"));
        assert!(!c.is_client_command("send_message"));
    }

    #[test]
    fn client_commands_are_marked() {
        let c = catalog();
        assert!(c.is_client_command("set_sticky"));
        assert!(c.is_client_command("del_sticky"));
    }
}
