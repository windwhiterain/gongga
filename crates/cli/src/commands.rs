//! Sticky command arguments and the two commands that manage them.
//!
//! `persistant_cmd_arguments[{name, value}]` is a client-side idea: the CLI
//! keeps them, fills them into every command before it goes out, and never
//! tells the server. They persist in `~/.gongga/cli.toml`, so a restarted client
//! still knows which session you were talking to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol::{Args, CommandSpec};
use serde::{Deserialize, Serialize};

/// `~/.gongga/cli.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    persistant_cmd_arguments: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    value: String,
}

pub struct Sticky {
    path: PathBuf,
    args: BTreeMap<String, String>,
}

impl Sticky {
    /// Loads from `home/cli.toml`, tolerating a missing or unreadable file: a
    /// client that cannot remember its defaults is still usable.
    pub fn load(home: &Path) -> Self {
        let path = home.join("cli.toml");
        let args = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<File>(&text)
                .map(|f| {
                    f.persistant_cmd_arguments
                        .into_iter()
                        .map(|e| (e.name, e.value))
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => BTreeMap::new(),
        };
        Self { path, args }
    }

    pub fn names(&self) -> Vec<String> {
        self.args.keys().cloned().collect()
    }

    /// `name` -> `value` pairs, for the status bar.
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.args.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn set(&mut self, name: &str, value: &str) -> anyhow::Result<()> {
        self.args.insert(name.to_string(), value.to_string());
        self.save()
    }

    /// Returns false when there was nothing to remove.
    pub fn del(&mut self, name: &str) -> anyhow::Result<bool> {
        match self.args.remove(name) {
            Some(_) => self.save().map(|()| true),
            None => Ok(false),
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File {
            persistant_cmd_arguments: self
                .args
                .iter()
                .map(|(name, value)| Entry { name: name.clone(), value: value.clone() })
                .collect(),
        };
        let text = toml::to_string_pretty(&file)?;
        std::fs::write(&self.path, text)?;
        Ok(())
    }

    /// Fills in any argument the caller omitted, when a sticky value exists.
    pub fn fill(&self, spec: Option<&CommandSpec>, args: &mut Args) {
        let Some(spec) = spec else { return };
        for a in &spec.args {
            if !args.contains_key(&a.name) {
                if let Some(v) = self.args.get(&a.name) {
                    args.insert(a.name.clone(), v.clone());
                }
            }
        }
    }
}

/// `--flag value` and `--flag=value`; nothing positional is accepted.
pub fn parse_args(tokens: &[String]) -> anyhow::Result<Args> {
    let mut args = Args::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        let Some(flag) = token.strip_prefix("--") else {
            anyhow::bail!("unexpected argument {token:?}; flags start with `--`");
        };
        if let Some((name, value)) = flag.split_once('=') {
            args.insert(name.to_string(), value.to_string());
            i += 1;
        } else {
            let value = tokens
                .get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("--{flag} needs a value"))?;
            args.insert(flag.to_string(), value.clone());
            i += 2;
        }
    }
    Ok(args)
}

/// Renders arguments back to a command line, for the echoed `$ ...` line.
pub fn render_args(args: &Args) -> String {
    args.iter()
        .map(|(k, v)| {
            if v.contains(' ') {
                format!(" --{k} \"{v}\"")
            } else {
                format!(" --{k} {v}")
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ArgSpec;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn sticky_arguments_survive_a_reload() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        let again = Sticky::load(d.path());
        assert_eq!(again.pairs(), vec![("session".to_string(), "main".to_string())]);
        assert_eq!(again.names(), vec!["session".to_string()]);
    }

    #[test]
    fn deleting_reports_whether_it_hit() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        assert!(s.del("session").unwrap());
        assert!(!s.del("session").unwrap());
    }

    #[test]
    fn fill_only_adds_missing_arguments() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        let spec = CommandSpec {
            name: "send_message".into(),
            summary: String::new(),
            args: vec![ArgSpec {
                name: "session".into(),
                summary: String::new(),
                required: false,
                value_hint: None,
                completions: protocol::CompletionSource::None,
            }],
        };
        let mut args = Args::new();
        s.fill(Some(&spec), &mut args);
        assert_eq!(args["session"], "main");
        let mut args = Args::from([("session".to_string(), "scratch".to_string())]);
        s.fill(Some(&spec), &mut args);
        assert_eq!(args["session"], "scratch");
    }

    #[test]
    fn both_flag_forms_parse() {
        assert_eq!(parse_args(&["--session".into(), "main".into()]).unwrap()["session"], "main");
        assert_eq!(parse_args(&["--session=main".into()]).unwrap()["session"], "main");
    }

    #[test]
    fn dangling_flags_are_rejected() {
        assert!(parse_args(&["--session".into()]).is_err());
        assert!(parse_args(&["oops".into()]).is_err());
    }
}
