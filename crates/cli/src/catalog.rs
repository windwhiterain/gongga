//! The completion source the TUI asks, and nowhere else.
//!
//! See `docs/notes/cli-catalog.md`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::complete::{Candidate, Completer, token_bounds};
use protocol::{ArgSpec, CommandSpec, CommandTarget, CompletionSource};

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
    filter_names: Vec<String>,
    /// Where a relative path value is read from: the client's own directory.
    cwd: PathBuf,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            published: Vec::new(),
            sessions: Vec::new(),
            sticky_names: Vec::new(),
            filter_names: Vec::new(),
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

    /// The sticky arguments that exist, for the source the server names.
    pub fn set_sticky_names(&self, names: &[String]) {
        self.write().sticky_names = names.to_vec();
    }

    /// The same for `del_event_filter --name`.
    pub fn set_filter_names(&self, names: &[String]) {
        self.write().filter_names = names.to_vec();
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
    filters: &'a [String],
    cwd: &'a Path,
    all: &'a [CommandSpec],
}

impl Completer for Catalog {
    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
        // `/` starts a command; anything else is a message, which offers
        // nothing. See `docs/notes/cli-catalog.md`.
        if !line.trim_start().starts_with('/') {
            return Vec::new();
        }
        let (_, _, token) = token_bounds(line, cursor);
        let words = shell_words::split(line).unwrap_or_default();
        let position = line[..cursor.min(line.len())].split_whitespace().count();
        let (all, sessions, sticky, filters, cwd) = {
            let inner = self.read();
            (
                inner.published.clone(),
                inner.sessions.clone(),
                inner.sticky_names.clone(),
                inner.filter_names.clone(),
                inner.cwd.clone(),
            )
        };
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
                .map(|a| Candidate::word(format!("--{}", a.name), a.summary.clone()))
                .collect();
        }

        // Command names, only while the command itself is still unwritten.
        if position == 1 && token.starts_with('/') {
            let prefix = token.trim_start_matches('/');
            return all
                .iter()
                .filter(|c| c.name.starts_with(prefix))
                .map(|c| Candidate::word(format!("/{}", c.name), c.summary.clone()))
                .collect();
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
        let ctx = Ctx {
            sessions: &sessions,
            sticky: &sticky,
            filters: &filters,
            cwd: &cwd,
            all: &all,
        };
        resolve(&spec.completions, &token, &ctx, &words, 0)
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
        CompletionSource::EventFilters => {
            pick(ctx.filters.iter().map(String::as_str), token, "filter")
        }
        CompletionSource::OfNamedArgument if depth < 2 => {
            let Some(named) = flag_value(words, "name") else {
                return Vec::new();
            };
            let found = ctx
                .all
                .iter()
                .flat_map(|c| c.args.iter())
                .find(|a| {
                    a.name == named
                        && !matches!(
                            a.completions,
                            CompletionSource::None | CompletionSource::OfNamedArgument
                        )
                })
                .map(|a| a.completions.clone());
            match found {
                Some(source) => resolve(&source, token, ctx, words, depth + 1),
                None => Vec::new(),
            }
        }
        CompletionSource::OfNamedArgument => Vec::new(),
    }
}

fn pick<'a>(values: impl Iterator<Item = &'a str>, prefix: &str, detail: &str) -> Vec<Candidate> {
    values
        .filter(|v| v.starts_with(prefix))
        .map(|v| Candidate::word(v, detail))
        .collect()
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
    summary: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> ArgSpec {
    ArgSpec {
        name: name.into(),
        summary: summary.into(),
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
        c.set_filter_names(&[]);
        let mut published = protocol::client_commands();
        published.extend(vec![
            CommandSpec {
                target: CommandTarget::Server,
                name: "send_message".into(),
                summary: "Send a message.".into(),
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
                summary: "Create or configure.".into(),
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
        assert_eq!(values(&c, "/set_st"), vec!["/set_sticky_cmd_argument"]);
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

    #[test]
    fn free_form_values_yield_nothing() {
        let c = catalog();
        assert!(values(&c, "/send_message --message ").is_empty());
    }

    #[test]
    fn sticky_names_complete_against_state() {
        let c = catalog();
        assert_eq!(
            values(&c, "/del_sticky_cmd_argument --name "),
            vec!["session"]
        );
    }

    /// `--name` takes an argument name, so it offers the catalog's, once each,
    /// whichever command declared them.
    #[test]
    fn argument_names_complete_from_the_whole_catalog() {
        let c = catalog();
        let got = values(&c, "/set_sticky_cmd_argument --name ");
        assert_eq!(
            got,
            vec!["cwd", "message", "mode", "name", "session", "split", "value"]
        );
    }

    /// A sticky value completes like the argument it fills: `session` offers
    /// sessions.
    #[test]
    fn a_sticky_value_completes_like_the_argument_it_fills() {
        let c = catalog();
        assert_eq!(
            values(&c, "/set_sticky_cmd_argument --name session --value "),
            vec!["main", "scratch"]
        );
        assert_eq!(
            values(&c, "/set_sticky_cmd_argument --name=session --value ma"),
            vec!["main"]
        );
    }

    /// A free-form argument stays free-form, and so does anything the catalog
    /// does not know.
    #[test]
    fn a_sticky_value_for_a_free_form_argument_offers_nothing() {
        let c = catalog();
        assert!(values(&c, "/set_sticky_cmd_argument --name message --value ").is_empty());
        assert!(values(&c, "/set_sticky_cmd_argument --value main").is_empty());
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

    /// `--name` takes an event field, so it offers the fields an event has.
    #[test]
    fn event_filter_names_complete_from_the_event_vocabulary() {
        let c = catalog();
        let got = values(&c, "/set_event_filter --name ");
        assert!(got.contains(&"session".to_string()), "{got:?}");
        assert!(got.contains(&"level".to_string()), "{got:?}");
        assert_eq!(
            values(&c, "/set_event_filter --name sess"),
            vec!["session", "sessions"]
        );
        assert!(values(&c, "/set_event_filter --name nope").is_empty());
    }

    /// The value is judged like the argument it mirrors: a session filter takes
    /// sessions.
    #[test]
    fn a_filter_value_completes_like_the_argument_it_mirrors() {
        let c = catalog();
        assert_eq!(
            values(&c, "/set_event_filter --name session --value "),
            vec!["main", "scratch"]
        );
    }

    /// The removal candidate comes from state, and follows it.
    #[test]
    fn del_event_filter_completes_against_the_filters_that_exist() {
        let c = catalog();
        assert!(values(&c, "/del_event_filter --name ").is_empty());
        c.set_filter_names(&["session".to_string()]);
        assert_eq!(values(&c, "/del_event_filter --name "), vec!["session"]);
    }

    /// The new command completes like every other one: name, argument, values.
    #[test]
    fn the_detail_command_completes_name_argument_and_modes() {
        let c = catalog();
        // Order follows the catalog, which is not what this is about.
        let mut both = values(&c, "/set_event");
        both.sort();
        assert_eq!(
            both,
            vec!["/set_event_detail", "/set_event_filter", "/set_event_split"]
        );
        assert_eq!(values(&c, "/set_event_det"), vec!["/set_event_detail"]);
        assert_eq!(values(&c, "/set_event_detail --"), vec!["--mode"]);
        assert_eq!(values(&c, "/set_event_detail --mode "), vec!["all", "line"]);
        // `one` was a mode; splitting is a command of its own now.
        assert!(values(&c, "/set_event_detail --mode o").is_empty());
        assert_eq!(values(&c, "/set_event_split --"), vec!["--split"]);
        assert_eq!(values(&c, "/set_event_split --split "), vec!["true", "false"]);
    }

    #[test]
    fn event_filters_are_client_commands() {
        let c = catalog();
        assert!(c.is_client_command("set_event_filter"));
        assert!(c.is_client_command("del_event_filter"));
    }

    #[test]
    fn client_commands_are_marked() {
        let c = catalog();
        assert!(c.is_client_command("set_sticky_cmd_argument"));
        assert!(!c.is_client_command("send_message"));
    }
}
