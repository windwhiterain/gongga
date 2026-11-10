import io

def edit(path, pairs):
    s = io.open(path, encoding="utf-8").read()
    for old, new in pairs:
        assert s.count(old) == 1, (path, old[:60], s.count(old))
        s = s.replace(old, new)
    io.open(path, "w", encoding="utf-8", newline="\n").write(s)

# --- remotes.rs: a read that reports why, and the file as this process follows it.
edit("crates/server/src/remotes.rs", [
(
'''/// The remotes in `file`, in the order the file writes them.
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
''',
'''/// The remotes in `file`, in the order the file writes them.
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
'''),
(
'''pub fn load(answer_timeout: Duration) -> Vec<RemoteConfig> {
    read(&path(), answer_timeout)
}
''',
'''pub fn load(answer_timeout: Duration) -> Vec<RemoteConfig> {
    let file = path();
    read(&file, answer_timeout).unwrap_or_else(|e| {
        tracing::warn!(file = %file.display(), error = %e, "could not read the remotes file");
        Vec::new()
    })
}
'''),
])

# --- session.rs: hold the catalog, not its first read.
edit("crates/server/src/session.rs", [
(
"    remotes: Vec<crate::remotes::RemoteConfig>,",
"    remotes: std::sync::Arc<crate::remotes::Catalog>,",
),
(
"        remotes: Vec<crate::remotes::RemoteConfig>,",
"        remotes: std::sync::Arc<crate::remotes::Catalog>,",
),
(
'''    /// The remote names a session may name, in the config's order.
    pub fn remote_names(&self) -> Vec<String> {
        self.remotes.iter().map(|r| r.name.clone()).collect()
    }
''',
'''    /// The remote names a session may name, in the file's order.
    pub fn remote_names(&self) -> Vec<String> {
        self.remotes.names()
    }
'''),
(
'''        match self.remotes.iter().find(|r| r.name == name) {
            Some(remote) => Ok(remote.endpoint.clone()),
            None => anyhow::bail!(
                "no remote named {name:?}; this server knows {}",
                if self.remotes.is_empty() {
                    "no remotes at all".to_string()
                } else {
                    self.remotes
                        .iter()
                        .map(|t| format!("{:?}", t.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        }
''',
'''        match self.remotes.find(name) {
            Some(remote) => Ok(remote.endpoint.clone()),
            None => {
                let known = self.remotes.names();
                anyhow::bail!(
                    "no remote named {name:?}; this server knows {}",
                    if known.is_empty() {
                        "no remotes at all".to_string()
                    } else {
                        known
                            .iter()
                            .map(|t| format!("{t:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                )
            }
        }
'''),
])

# --- lib.rs: one catalog, handed to the sessions.
edit("crates/server/src/lib.rs", [
(
"        crate::remotes::load(cfg.endpoint.answer_timeout),",
"        crate::remotes::Catalog::load(cfg.endpoint.answer_timeout),",
),
])
print("ok")
