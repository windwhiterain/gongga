//! The machines a session's shells may run on.
//!
//! See `docs/notes/server-remotes.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::remote::{Endpoint, Reach};

/// One machine a session may name as its `--remote`.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// What `set_session --remote` names.
    pub name: String,
    pub endpoint: Endpoint,
}

/// The on-disk shape, shared with the warning about a stale `config.toml`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct FileRemote {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) identity: Option<PathBuf>,
    pub(crate) runner: Option<String>,
}

/// The file as it is written: one table per remote.
#[derive(Debug, Default, Deserialize, Serialize)]
struct File {
    /// `[targets.<name>]`: where these lived before the word changed.
    #[serde(default, alias = "targets")]
    remotes: BTreeMap<String, FileRemote>,
}

/// Where this machine's remotes are: an override, else `~/.agents/remotes.toml`.
///
/// # Invariant
///
/// Not the home's. `~/.agents/` is the machine's own directory -- never a repository,
/// which is what `keys.toml` relies on too -- so a host name and a private key path
/// cannot be published by a commit that was only meant to ship a skill.
pub fn path() -> PathBuf {
    match std::env::var("GONGGA_REMOTES_CATALOG") {
        Ok(over) if !over.is_empty() => PathBuf::from(over),
        _ => crate::agents::Roots::of().private.join("remotes.toml"),
    }
}

/// Every remote this machine configures; a table that cannot be used is dropped
/// with a line saying why.
///
/// # Invariant
///
/// `answer_timeout` is this home's `[remote] answer_timeout_ms`, handed to every
/// table here: a peer's silence is bounded, and what outlives it is an error.
pub fn load(answer_timeout: Duration) -> Vec<RemoteConfig> {
    let file = path();
    read(&file, answer_timeout).unwrap_or_else(|e| {
        tracing::warn!(file = %file.display(), error = %e, "could not read the remotes file");
        Vec::new()
    })
}

/// The remotes in `file`, in the order the file writes them.
///
/// No file is no remote. Anything else that cannot be read is the caller's to
/// decide about: a file mid-edit must not be mistaken for a machine with no peers.
pub fn read(file: &Path, answer_timeout: Duration) -> anyhow::Result<Vec<RemoteConfig>> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => anyhow::bail!("reading {}: {e}", file.display()),
    };
    let parsed: File = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not TOML: {e}", file.display()))?;
    Ok(parsed
        .remotes
        .into_iter()
        .filter_map(|(name, one)| one.into_config(name.trim().to_string(), answer_timeout))
        .collect())
}

/// The remotes file as this process follows it.
///
/// # Invariant
///
/// A name resolves against the file as it is now. The file belongs to the machine,
/// and publishing a peer is an edit, never a restart; a file that stops parsing
/// keeps the remotes that last parsed. See `docs/notes/server-remotes.md`.
pub struct Catalog {
    file: PathBuf,
    answer: Duration,
    live: crate::live::Live<Vec<RemoteConfig>>,
}

impl Catalog {
    pub fn load(answer: Duration) -> std::sync::Arc<Self> {
        let file = path();
        let first = read(&file, answer).unwrap_or_else(|e| {
            tracing::warn!(file = %file.display(), error = %e, "could not read the remotes file");
            Vec::new()
        });
        std::sync::Arc::new(Self {
            live: crate::live::Live::of(vec![file.clone()], first),
            file,
            answer,
        })
    }

    /// Every remote the file has now.
    fn all(&self) -> Vec<RemoteConfig> {
        self.live.current(|| read(&self.file, self.answer))
    }

    /// The names a session may name, in the file's order.
    pub fn names(&self) -> Vec<String> {
        self.all().into_iter().map(|remote| remote.name).collect()
    }

    /// One remote by the name a call gave.
    pub fn find(&self, name: &str) -> Option<RemoteConfig> {
        self.all().into_iter().find(|remote| remote.name == name)
    }
}

impl FileRemote {
    /// One table as a remote, or nothing and a line saying what is wrong with it.
    ///
    /// # Invariant
    ///
    /// A refused table is absent, never a local fallback: a session that asked for a
    /// peer must not quietly get this machine. See `docs/notes/server-remote.md`.
    fn into_config(self, name: String, answer_timeout: Duration) -> Option<RemoteConfig> {
        if name.is_empty() || name == "local" || name.eq_ignore_ascii_case("none") {
            tracing::warn!(
                %name,
                "a remote cannot be called `local` or `none`: those are this machine"
            );
            return None;
        }
        let host = self.host.unwrap_or_default();
        if host.trim().is_empty() {
            tracing::warn!(%name, "a remote needs a `host`");
            return None;
        }
        // Absent means this build's own, placed on the peer and greeted.
        if self
            .runner
            .as_ref()
            .is_some_and(|runner| runner.trim().is_empty())
        {
            tracing::warn!(%name, "a remote's `runner` is empty; leave it out instead");
            return None;
        }
        let runner = self.runner.unwrap_or_default();
        Some(RemoteConfig {
            name,
            endpoint: Endpoint::Remote {
                transport: Reach::Ssh {
                    host,
                    port: self.port,
                    identity: self.identity,
                },
                runner,
                answer: answer_timeout,
            },
        })
    }
}

/// The remote tables a `config.toml` still carries, and so the ones this server
/// no longer reads.
///
/// # Invariant
///
/// Loud, never silent. A home whose `config.toml` still names a peer has remotes
/// this build does not have, and a session on one of those would be refused with
/// "no remote named ..." -- a message that says nothing about the file it is in.
/// See `docs/notes/server-migrate.md`.
pub fn stranded(config_path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(config_path) else {
        return Vec::new();
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    // Both spellings: `[targets]` is where these lived before the word changed.
    ["remotes", "targets"]
        .into_iter()
        .filter_map(|table| value.get(table).and_then(toml::Value::as_table))
        .flat_map(|table| table.keys().cloned())
        .collect()
}
