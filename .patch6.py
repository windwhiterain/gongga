import io

def edit(path, pairs):
    s = io.open(path, encoding="utf-8").read()
    for old, new, n in pairs:
        assert s.count(old) == n, (path, old[:50], s.count(old))
        s = s.replace(old, new)
    io.open(path, "w", encoding="utf-8", newline="\n").write(s)

# The initial value is what placed the process; only `Current` needs it.
edit("crates/server/src/live.rs", [
('''    /// The newest value: `reload` runs only when a file says it changed.''',
'''    /// The value this process started with: what placed it, and cannot move.
    pub(crate) fn initial(&self) -> T {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .value
            .clone()
    }

    /// The newest value: `reload` runs only when a file says it changed.''', 1),
])

edit("crates/server/src/config.rs", [
('''/// A 128-bit random key, hex encoded.''',
'''/// The home's `config.toml` as this process follows it.
///
/// # Invariant
///
/// Everything a request reads comes from [`Current::now`]: the file is the machine's,
/// and changing a model, a timeout or a shell is an edit, never a restart. What
/// placed this process -- `home`, `db_path`, `bind`, `key` -- is
/// [`Current::anchor`], read once, because a database and a listening socket do not
/// move. See `docs/notes/server-config.md`.
pub struct Current {
    config_path: PathBuf,
    home: Option<PathBuf>,
    live: crate::live::Live<std::sync::Arc<Config>>,
}

impl Current {
    /// Reads the config once, and follows it from then on.
    pub fn load(
        config_path: Option<&Path>,
        home: Option<&Path>,
    ) -> anyhow::Result<std::sync::Arc<Self>> {
        let config = Config::load_with_home(config_path, home)?;
        Ok(std::sync::Arc::new(Self {
            live: crate::live::Live::of(vec![config.config_path.clone()], std::sync::Arc::new(config.clone())),
            config_path: config.config_path,
            home: home.map(Path::to_path_buf),
        }))
    }

    /// The config the file has now: a value that did not change is the same `Arc`.
    pub fn now(&self) -> std::sync::Arc<Config> {
        let (config_path, home) = (self.config_path.clone(), self.home.clone());
        self.live.current(|| {
            Config::load_with_home(Some(&config_path), home.as_deref()).map(std::sync::Arc::new)
        })
    }

    /// The config this process started with.
    pub fn anchor(&self) -> std::sync::Arc<Config> {
        self.live.initial()
    }
}

/// A 128-bit random key, hex encoded.''', 1),
])

# session.rs: the manager asks the file, not the snapshot it was built from.
edit("crates/server/src/session.rs", [
('''    bash: BashConfig,''', '''    config: std::sync::Arc<crate::config::Current>,''', 1),
('''        bash: BashConfig,
        model: String,''', '''        config: std::sync::Arc<crate::config::Current>,''', 1),
('''            bash,
            model,
''', '', 1),
('''        let source = model.unwrap_or(&self.model);''',
 '''        let current = self.config.now();
        let source = model.map(str::to_owned).unwrap_or_else(|| current.model.model());
        let source = source.as_str();''', 1),
('''        let program = self
            .bash
            .program''',
 '''        let current = self.config.now();
        let program = current
            .bash
            .program''', 1),
])
print("ok")
