//! The session list this window drew last, on disk.
//!
//! See `docs/notes/tui-roster.md`.
//!
//! # Invariant
//!
//! A view, never a claim: a row says a name was in the list and how its pane was
//! titled, and nothing about a transcript. A greeting replaces the list outright,
//! so this is a record of the past and not a second source of truth.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use client::Listed;

/// One session the list held, as the frame drew it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    name: String,
    status: String,
    title: String,
}

impl Row {
    fn of(listed: &Listed) -> Self {
        Self {
            name: listed.name.clone(),
            status: listed.status.clone(),
            title: listed.title.clone(),
        }
    }

    fn listed(&self) -> Listed {
        Listed {
            name: self.name.clone(),
            status: self.status.clone(),
            title: self.title.clone(),
        }
    }
}

/// The list of one endpoint, and the file it came from.
///
/// # Invariant
///
/// One file per endpoint, named by the address this client would dial: the same
/// key the journals use, so two servers are two lists and the answer is known
/// before any server speaks. See `docs/notes/tui-cache.md`.
pub struct Roster {
    root: PathBuf,
    endpoint: String,
    /// What this process last wrote, so a frame that changed nothing writes nothing.
    known: Vec<Row>,
}

impl Roster {
    /// The record of `endpoint` under this client's own root.
    pub fn new(root: &Path, endpoint: &str) -> Self {
        let mut roster = Self {
            root: root.to_path_buf(),
            endpoint: endpoint.to_string(),
            known: Vec::new(),
        };
        roster.known = roster.read();
        roster
    }

    /// Points at another endpoint's record: `connect_server` moved the address.
    ///
    /// # Invariant
    ///
    /// Nothing is carried across: the rows of one server are not a guess about
    /// another's, so the list this window draws next is the one on disk there.
    pub fn repoint(&mut self, root: &Path, endpoint: &str) {
        self.root = root.to_path_buf();
        self.endpoint = endpoint.to_string();
        self.known = self.read();
    }

    /// The rows to draw before a greeting: empty when this endpoint has none.
    ///
    /// # Invariant
    ///
    /// A status of `error` is read back as `error` and every other word as idle:
    /// a turn was running when the window closed, and a list that says so for a
    /// server nobody can reach would be a claim the record cannot make.
    pub fn load(&self) -> Vec<Listed> {
        self.known
            .iter()
            .map(|row| Listed {
                status: match row.status.as_str() {
                    "error" => "error".to_string(),
                    _ => "idle".to_string(),
                },
                ..row.listed()
            })
            .collect()
    }

    /// Writes down the list a frame drew, if it changed.
    ///
    /// # Invariant
    ///
    /// The whole file, written here and nowhere else: a list drawn from the server
    /// is the list a later window starts from, and a partial write would leave two
    /// writers racing for one file. A failure is the caller's to report -- a window
    /// that cannot record its list still draws one.
    pub fn know(&mut self, listed: &[Listed]) -> anyhow::Result<()> {
        let rows: Vec<Row> = listed.iter().map(Row::of).collect();
        if rows == self.known {
            return Ok(());
        }
        let path = self.path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File {
            session: rows.clone(),
        };
        std::fs::write(&path, toml::to_string_pretty(&file)?)?;
        self.known = rows;
        Ok(())
    }

    fn path(&self) -> PathBuf {
        self.root.join("roster").join(format!(
            "{}.toml",
            client::window_cache::escaped(&self.endpoint)
        ))
    }

    fn read(&self) -> Vec<Row> {
        let Ok(text) = std::fs::read_to_string(self.path()) else {
            return Vec::new();
        };
        toml::from_str::<File>(&text)
            .map(|file| file.session)
            .unwrap_or_default()
    }
}

/// The file's shape: one key, so a later field cannot be mistaken for a row.
#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    session: Vec<Row>,
}
