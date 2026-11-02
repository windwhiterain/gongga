//! A peer's files, read without borrowing a session's shell.
//!
//! See `docs/notes/server-remote.md`.

use std::sync::Mutex;

use tokio::process::Command;

use crate::remote::endpoint::Endpoint;
use crate::remote::transport::{DirEntry, Runner};

/// One remote, and the connection kept open to read its files.
pub struct Reader {
    endpoint: Endpoint,
    /// The live connection, if there is one; dropped and re-dialled when it dies.
    runner: Mutex<Option<Runner>>,
}

impl Reader {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            runner: Mutex::new(None),
        }
    }

    pub fn describe(&self) -> String {
        self.endpoint.describe()
    }

    /// The whole of one file on the peer.
    ///
    /// # Invariant
    ///
    /// A dead connection costs one re-dial for the whole read, never a failed read
    /// that a caller would have to know to retry: the file a message names may be
    /// the first thing asked for after a stall, and a message is not a place to
    /// explain a transport.
    pub async fn read(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        match self.read_whole(path).await {
            Ok(bytes) => Ok(bytes),
            Err(first) => {
                self.forget();
                self.read_whole(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("after one re-dial, {first}; then {e}"))
            }
        }
    }

    /// The names in one peer directory, read the same way.
    pub async fn list(&self, path: &str) -> anyhow::Result<Vec<DirEntry>> {
        match self.list_once(path).await {
            Ok(entries) => Ok(entries),
            Err(first) => {
                self.forget();
                self.list_once(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("after one re-dial, {first}; then {e}"))
            }
        }
    }

    async fn read_whole(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut offset = 0;
        loop {
            let runner = self.runner().await?;
            let chunk = runner.read(path, offset).await?;
            bytes.extend_from_slice(&chunk.data);
            if chunk.eof || chunk.data.is_empty() {
                return Ok(bytes);
            }
            offset = chunk.next;
        }
    }

    async fn list_once(&self, path: &str) -> anyhow::Result<Vec<DirEntry>> {
        self.runner().await?.list(path).await
    }

    /// The kept connection, dialled if there is none or the old one has died.
    async fn runner(&self) -> anyhow::Result<Runner> {
        let held = {
            let mut held = self.runner.lock().unwrap_or_else(|e| e.into_inner());
            match held.as_ref().filter(|runner| runner.connected()) {
                Some(runner) => return Ok(runner.clone()),
                None => held.take(),
            }
        };
        drop(held);
        let command: Command = self.endpoint.runner_command()?;
        let fresh = Runner::spawn(command)
            .await
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.describe()))?;
        *self.runner.lock().unwrap_or_else(|e| e.into_inner()) = Some(fresh.clone());
        Ok(fresh)
    }

    /// Drops the kept connection, so the next ask dials again.
    fn forget(&self) {
        self.runner.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // The runner ends its own shells when its stdin closes; nothing here owns one.
        self.runner.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}
