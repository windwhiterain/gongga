//! The completion source the TUI asks, and nowhere else.
//!
//! The TUI supplies the token; this decides what could go there. It merges two
//! catalogs: the server's, published at connect time, and the client's own
//! (`set_persistant_cmd_argument` and friends), which never leave the process.

use std::sync::{Arc, RwLock};

use protocol::{ArgSpec, CommandSpec, CompletionSource};
use tui::{Candidate, Completer};

/// A catalog that can be swapped while the TUI holds a reference to it.
#[derive(Clone, Default)]
pub struct Catalog(Arc<RwLock<Inner>>);

#[derive(Default)]
struct Inner {
    server: Vec<CommandSpec>,
    client: Vec<CommandSpec>,
    sessions: Vec<String>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_server_commands(&self, commands: Vec<CommandSpec>) {
        self.write().server = commands;
    }

    pub fn set_sessions(&self, sessions: Vec<String>) {
        self.write().sessions = sessions;
    }

    /// Rebuilds the client-side specs, whose `--name` completion depends on the
    /// sticky arguments that currently exist.
    pub fn set_sticky_names(&self, names: &[String]) {
        self.write().client = client_specs(names);
    }

    pub fn commands(&self) -> Vec<CommandSpec> {
        let inner = self.read();
        let mut all = inner.client.clone();
        all.extend(inner.server.iter().cloned());
        all
    }

    pub fn command(&self, name: &str) -> Option<CommandSpec> {
        self.commands().into_iter().find(|c| c.name == name)
    }

    pub fn is_client_command(&self, name: &str) -> bool {
        self.read().client.iter().any(|c| c.name == name)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl Completer for Catalog {
    fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate> {
        let (_, _, token) = tui::token_bounds(line, cursor);
        let words = shell_words::split(line).unwrap_or_default();
        let position = line[..cursor.min(line.len())].split_whitespace().count();
        let (server, client, sessions) = {
            let inner = self.read();
            (
                inner.server.clone(),
                inner.client.clone(),
                inner.sessions.clone(),
            )
        };
        let all: Vec<CommandSpec> = client.iter().cloned().chain(server.iter().cloned()).collect();

        if let Some(prefix) = token.strip_prefix("--") {
            let cmd = words.first().map(|t| t.trim_start_matches('/'));
            let args: &[ArgSpec] = cmd
                .and_then(|c| all.iter().find(|s| s.name == c))
                .map(|c| c.args.as_slice())
                .unwrap_or(&[]);
            return args
                .iter()
                .filter(|a| a.name.starts_with(prefix))
                .map(|a| Candidate {
                    insert: format!("--{}", a.name),
                    detail: a.summary.clone(),
                })
                .collect();
        }

        if position <= 1 {
            let prefix = token.trim_start_matches('/');
            return all
                .iter()
                .filter(|c| c.name.starts_with(prefix))
                .map(|c| Candidate {
                    insert: c.name.clone(),
                    detail: c.summary.clone(),
                })
                .collect();
        }

        // Past the command name: only a flag's value, and only when the spec
        // says where the candidates come from.
        let Some(prev) = words.get(position.wrapping_sub(1)) else {
            return Vec::new();
        };
        let Some(flag) = prev.strip_prefix("--") else {
            return Vec::new();
        };
        let cmd = words.first().map(|t| t.trim_start_matches('/'));
        let Some(spec) = cmd
            .and_then(|c| all.iter().find(|s| s.name == c))
            .and_then(|c| c.args.iter().find(|a| a.name == flag))
        else {
            return Vec::new();
        };
        match &spec.completions {
            CompletionSource::None => Vec::new(),
            CompletionSource::Sessions => sessions
                .iter()
                .filter(|s| s.starts_with(token.as_str()))
                .map(|s| Candidate { insert: s.clone(), detail: "session".into() })
                .collect(),
            CompletionSource::Static(values) => values
                .iter()
                .filter(|v| v.starts_with(token.as_str()))
                .map(|v| Candidate { insert: v.clone(), detail: "value".into() })
                .collect(),
        }
    }
}

/// The commands the CLI handles itself. Their arguments complete against the
/// sticky arguments that exist right now, so the specs are rebuilt on change.
fn client_specs(sticky: &[String]) -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "set_persistant_cmd_argument".into(),
            summary: "Set a sticky argument applied to every later command.".into(),
            args: vec![
                arg("name", "Argument name, e.g. `session`.", true, Some("name"), CompletionSource::None),
                arg("value", "Value to apply when the flag is omitted.", true, Some("value"), CompletionSource::None),
            ],
        },
        CommandSpec {
            name: "del_persistant_cmd_argument".into(),
            summary: "Remove a sticky argument.".into(),
            args: vec![arg(
                "name",
                "Argument name to remove.",
                true,
                Some("name"),
                CompletionSource::Static(sticky.to_vec()),
            )],
        },
    ]
}

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

    fn catalog() -> Catalog {
        let c = Catalog::new();
        c.set_sticky_names(&["session".to_string()]);
        c.set_server_commands(vec![CommandSpec {
            name: "send_message".into(),
            summary: "Send a message.".into(),
            args: vec![
                arg("message", "The text.", true, None, CompletionSource::None),
                arg("session", "Target.", false, None, CompletionSource::Sessions),
            ],
        }]);
        c.set_sessions(vec!["main".into(), "scratch".into()]);
        c
    }

    #[test]
    fn completes_command_names_from_both_catalogs() {
        let c = catalog();
        let names: Vec<String> = c.complete("set", 3).into_iter().map(|x| x.insert).collect();
        assert_eq!(names, vec!["set_persistant_cmd_argument"]);
        let names: Vec<String> = c.complete("send", 4).into_iter().map(|x| x.insert).collect();
        assert_eq!(names, vec!["send_message"]);
    }

    #[test]
    fn completes_flag_names() {
        let c = catalog();
        let line = "send_message --s";
        let got: Vec<String> = c
            .complete(line, line.len())
            .into_iter()
            .map(|x| x.insert)
            .collect();
        assert_eq!(got, vec!["--session"]);
    }

    #[test]
    fn completes_flag_values_from_sessions() {
        let c = catalog();
        let line = "send_message --session ";
        let got: Vec<String> = c
            .complete(line, line.len())
            .into_iter()
            .map(|x| x.insert)
            .collect();
        assert_eq!(got, vec!["main", "scratch"]);
    }

    #[test]
    fn free_form_values_yield_nothing() {
        let c = catalog();
        let line = "send_message --message ";
        assert!(c.complete(line, line.len()).is_empty());
    }

    #[test]
    fn sticky_names_complete_against_state() {
        let c = catalog();
        let line = "del_persistant_cmd_argument --name ";
        let got: Vec<String> = c
            .complete(line, line.len())
            .into_iter()
            .map(|x| x.insert)
            .collect();
        assert_eq!(got, vec!["session"]);
    }

    #[test]
    fn client_commands_are_marked() {
        let c = catalog();
        assert!(c.is_client_command("set_persistant_cmd_argument"));
        assert!(!c.is_client_command("send_message"));
    }
}
