//! A file this process follows, rather than one it read once.
//!
//! See `docs/notes/server-config.md`.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// What a file looked like when it was read: enough to notice an edit without
/// hashing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    size: u64,
    modified: Option<std::time::SystemTime>,
}

impl Stamp {
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// What each of these files looked like when it was read.
pub(crate) fn stamps(paths: &[PathBuf]) -> Vec<Option<Stamp>> {
    paths.iter().map(|path| Stamp::of(path)).collect()
}

/// Something built *from* a config file, kept until the file is read again.
///
/// # Invariant
///
/// Keyed on the read that built it ([`crate::config::Config::read`]), never on the
/// value coming back equal: a build may touch the filesystem, so it runs once per
/// edit rather than once per call, and a call in flight keeps the value it started
/// with.
pub struct Derived<C, T> {
    build: Box<dyn Fn(&C) -> T + Send + Sync>,
    current: RwLock<Option<(u64, T)>>,
}

impl<C, T: Clone> Derived<C, T> {
    pub fn new(build: impl Fn(&C) -> T + Send + Sync + 'static) -> Self {
        Self {
            build: Box::new(build),
            current: RwLock::new(None),
        }
    }

    /// The value this read describes, built now if this is a read it has not seen.
    pub fn of(&self, read: u64, config: &C) -> T {
        if let Ok(current) = self.current.read()
            && let Some((built, value)) = current.as_ref()
            && *built == read
        {
            return value.clone();
        }
        // Built outside the lock: a racing pair builds the same value, which is
        // cheaper than queueing every reader behind one.
        let value = (self.build)(config);
        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
        *current = Some((read, value.clone()));
        value
    }
}

/// One value, kept current with the files it was read from.
///
/// # Invariant
///
/// A file that stops parsing is an edit in flight, so the value that last parsed
/// keeps serving: an edit must never cost a running process what it already had.
pub(crate) struct Live<T> {
    paths: Vec<PathBuf>,
    state: RwLock<State<T>>,
}

struct State<T> {
    /// The stamps of the read `value` came from, or of the read that failed.
    stamps: Vec<Option<Stamp>>,
    value: T,
}

impl<T: Clone> Live<T> {
    /// A value with the stamps it was read from.
    pub(crate) fn of(paths: Vec<PathBuf>, value: T) -> Self {
        let stamps = stamps(&paths);
        Self {
            paths,
            state: RwLock::new(State { stamps, value }),
        }
    }

    /// The value this process started with: what placed it, and cannot move.
    pub(crate) fn initial(&self) -> T {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .value
            .clone()
    }

    /// The newest value: `reload` runs only when a file says it changed.
    pub(crate) fn current<F>(&self, reload: F) -> T
    where
        F: FnOnce() -> anyhow::Result<T>,
    {
        let now = stamps(&self.paths);
        if let Ok(state) = self.state.read()
            && state.stamps == now
        {
            return state.value.clone();
        }
        let fresh = reload();
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.stamps = now;
        match fresh {
            Ok(value) => {
                state.value = value;
            }
            Err(e) => tracing::warn!(
                files = ?self.paths,
                error = %e,
                "a config file did not parse; the last good read stands"
            ),
        }
        state.value.clone()
    }
}
