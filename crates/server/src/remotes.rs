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
    read(&path(), answer_timeout)
}

/// The remotes in `file`, in the order the file writes them.
pub fn read(file: &Path, answer_timeout: Duration) -> Vec<RemoteConfig> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(file = %file.display(), error = %e, "could not read the remotes file");
            return Vec::new();
        }
    };
    let parsed: File = match toml::from_str(&text) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!(file = %file.display(), error = %e, "the remotes file is not TOML");
            return Vec::new();
        }
    };
    parsed
        .remotes
        .into_iter()
        .filter_map(|(name, one)| one.into_config(name.trim().to_string(), answer_timeout))
        .collect()
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
