//! One path space: the machine a session's paths live on, and what it reads and runs.
//!
//! See `docs/notes/server-space.md`.

use std::path::Path;
use std::sync::Mutex;

use tokio::process::Command;

use crate::remote::Endpoint;
use crate::remote::protocol::{Event, Stream};
use crate::remote::transport::{DirEntry, EndpointFile, Runner};

/// What one program printed, and what it exited with, on one path space.
///
/// # Invariant
///
/// The exit status is the program's own: `None` is a peer that ended without saying, or
/// a connection that died first -- never a guess of `0`, which a caller would read as
/// "it worked".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Run {
    pub exit: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Run {
    /// Whether it ran to its end and said it succeeded.
    pub fn succeeded(&self) -> bool {
        self.exit == Some(0)
    }

    /// Its standard output as text, for a program that prints text.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Where a session's paths live: this machine, or the peer they are on.
///
/// # Invariant
///
/// One value decides both, so no caller asks which machine it is on: a path handed to
/// [`Space::run`] is resolved by the machine that owns it, and a program is started
/// there. A peer is reached through one connection that is kept, so a message naming
/// twelve files costs one dial rather than twelve. See `docs/notes/server-space.md`.
#[derive(Clone)]
pub struct Space {
    endpoint: Endpoint,
    /// The kept connection, when there is one; dropped and re-dialled when it dies.
    link: std::sync::Arc<Mutex<Option<Runner>>>,
}

impl std::fmt::Debug for Space {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Space")
            .field("endpoint", &self.endpoint.describe())
            .finish()
    }
}

impl Space {
    /// This machine, named no way at all.
    pub fn local() -> Self {
        Self {
            endpoint: Endpoint::local(std::path::PathBuf::new()),
            link: std::sync::Arc::new(Mutex::new(None)),
        }
    }

    /// The space of one endpoint: its own machine when it is this one, else the peer.
    pub fn of(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            link: std::sync::Arc::new(Mutex::new(None)),
        }
    }

    /// Whether the paths here are this machine's.
    pub fn is_local(&self) -> bool {
        !self.endpoint.is_remote()
    }

    /// What a caller is told it ran on.
    pub fn describe(&self) -> String {
        self.endpoint.describe()
    }

    /// Which of a session's two machines these paths are on.
    pub fn machine(&self) -> crate::session::Machine {
        match self.is_local() {
            true => crate::session::Machine::Local,
            false => crate::session::Machine::Remote,
        }
    }

    /// The space of the named peer this machine configures, or this one when it names none.
    ///
    /// # Invariant
    ///
    /// A name this machine does not configure is not a peer: the answer is this space,
    /// because a reference that names an unknown machine must read as it would have
    /// without one -- never as a path this machine invented. The link is kept per peer in
    /// one table, so every reader of one remote shares one connection.
    /// See `docs/notes/server-meta.md`.
    pub fn peer(&self, name: &str) -> Self {
        let name = name.trim();
        if name.is_empty()
            || name.eq_ignore_ascii_case("none")
            || name.eq_ignore_ascii_case(protocol::local_machine())
        {
            return crate::space::Space::local();
        }
        let Some(configured) = crate::remotes::load(self.answer_timeout())
            .into_iter()
            .find(|remote| remote.name == name)
        else {
            return self.clone();
        };
        let mut peers = peers().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(space) = peers.get(name) {
            return space.clone();
        }
        let space = crate::space::Space::of(configured.endpoint.clone());
        peers.insert(name.to_string(), space.clone());
        space
    }

    /// How long this space's peer may stay silent, as this home configures it.
    fn answer_timeout(&self) -> std::time::Duration {
        crate::remote::transport::DEFAULT_ANSWER_TIMEOUT
    }

    /// The one path `named` means here, relative ones joined against `cwd`.
    ///
    /// # Invariant
    ///
    /// A peer's path is joined the way its own shell joins it, not this platform's:
    /// `\` becomes `/`, a drive letter reads as absolute, and a relative name hangs off
    /// the directory the caller stated rather than wherever this process stands.
    pub fn spell(&self, cwd: &Path, named: &str) -> String {
        match self.is_local() {
            true => crate::image::resolve(cwd, named)
                .to_string_lossy()
                .into_owned(),
            false => {
                let named = named.replace('\\', "/");
                let absolute = named.starts_with('/')
                    || named.char_indices().nth(1).is_some_and(|(at, c)| {
                        c == ':' && named.as_bytes()[0].is_ascii_alphabetic() && at == 1
                    });
                let home = cwd.to_string_lossy();
                if absolute || home.is_empty() {
                    return named;
                }
                format!("{}/{}", home.trim_end_matches('/'), named)
            }
        }
    }

    /// The whole of one file here, by the spelling [`Space::spell`] gave it.
    ///
    /// # Invariant
    ///
    /// A missing file is `None` rather than an error: every caller of this is resolving
    /// a reference, and a reference nothing can be read for stays literal in the text
    /// instead of failing what named it.
    pub async fn read_bytes(&self, path: &str) -> Option<Vec<u8>> {
        match self.is_local() {
            true => std::fs::read(path).ok(),
            false => {
                let mut bytes = Vec::new();
                let mut offset = 0u64;
                loop {
                    let chunk = self.link().await.ok()?.read(path, offset).await.ok()?;
                    bytes.extend_from_slice(&chunk.data);
                    if chunk.eof || chunk.data.is_empty() {
                        return Some(bytes);
                    }
                    // Every chunk moves the cursor: a runner that stopped advancing it
                    // would be read for ever.
                    if chunk.next <= offset {
                        return None;
                    }
                    offset = chunk.next;
                }
            }
        }
    }

    /// The names in one directory here, by a spelling [`Space::spell`] gave it.
    pub async fn list(&self, path: &str) -> Option<Vec<DirEntry>> {
        match self.is_local() {
            true => {
                let mut entries: Vec<DirEntry> = std::fs::read_dir(path)
                    .ok()?
                    .flatten()
                    .map(|entry| {
                        let kind = entry.file_type().ok();
                        // A link to a directory is listed as one: the community bucket
                        // is junctions, and a skipped junction hides its skills.
                        let linked_dir =
                            kind.is_some_and(|kind| kind.is_symlink()) && entry.path().is_dir();
                        DirEntry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            is_dir: linked_dir || kind.is_some_and(|kind| kind.is_dir()),
                            is_file: kind.is_some_and(|kind| kind.is_file()),
                        }
                    })
                    .collect();
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                Some(entries)
            }
            false => self.link().await.ok()?.list(path).await.ok(),
        }
    }

    /// The room left on this machine's volume, seen from one path here.
    ///
    /// # Invariant
    ///
    /// Asked through the same kept connection a reference is read on: a disk report
    /// after a tool call must not cost a dial of its own.
    pub async fn free_space(&self, path: &str) -> Option<u64> {
        match self.is_local() {
            true => crate::disk::free_space(std::path::Path::new(path)),
            false => self.link().await.ok()?.space(path).await.ok().flatten(),
        }
    }

    /// What this machine says is at one path, or nothing when it cannot be asked.
    pub async fn stat(&self, path: &str) -> Option<EndpointFile> {
        match self.is_local() {
            true => {
                let meta = std::fs::metadata(path).ok()?;
                Some(EndpointFile {
                    path: path.to_string(),
                    exists: true,
                    is_file: meta.is_file(),
                    is_dir: meta.is_dir(),
                    size: meta.len(),
                })
            }
            false => self.link().await.ok()?.stat(path).await.ok(),
        }
    }

    /// Runs one program to its end, standing in `cwd`, and answers what it printed.
    ///
    /// # Invariant
    ///
    /// `cwd` and `program` are spelled for the machine that owns them: a path this
    /// machine can open is not evidence about a peer's, and a program name is resolved
    /// by that machine's own `PATH`. No clock here: a caller with a deadline wraps this
    /// call in one, because only it knows what its command is worth waiting for.
    pub async fn run(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        stdin: Option<Vec<u8>>,
    ) -> Result<Run, String> {
        match self.is_local() {
            true => run_here(cwd, program, args, stdin).await,
            false => self.run_there(cwd, program, args, stdin).await,
        }
    }

    /// One program on the peer, through the connection this space keeps.
    async fn run_there(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        stdin: Option<Vec<u8>>,
    ) -> Result<Run, String> {
        let runner = self.link().await.map_err(|e| e.to_string())?;
        // The directory is passed, never resolved here: it is a path as that machine
        // spells it.
        let at = cwd.to_string_lossy().into_owned();
        let asked = (!at.trim().is_empty()).then_some(at.as_str());
        let mut shell = runner
            .open_with(asked, Some(program), Some(args))
            .await
            .map_err(|e| e.to_string())?;
        if let Some(bytes) = stdin {
            shell.write(&bytes).await.map_err(|e| e.to_string())?;
        }
        let mut run = Run::default();
        while let Some(event) = shell.next_event().await {
            match event {
                Event::Out { stream, data, .. } => {
                    let Some(bytes) = crate::base64::decode(&data) else {
                        continue;
                    };
                    match stream {
                        Stream::Stdout => run.stdout.extend_from_slice(&bytes),
                        Stream::Stderr => run.stderr.extend_from_slice(&bytes),
                    }
                }
                // Its pipes ended: this is the status the peer could prove.
                Event::End { code, .. } => {
                    run.exit = code;
                    break;
                }
                Event::Hello { .. } => continue,
            }
        }
        // The shell is this connection's, so it is given back before the connection goes.
        let _ = shell.close().await;
        Ok(run)
    }

    /// The kept connection, dialled if there is none or the old one has died.
    ///
    /// # Invariant
    ///
    /// A dead connection costs one re-dial for the whole read, never a failed read a
    /// caller would have to know to retry: the file a message names may be the first
    /// thing asked after a stall. See `docs/notes/server-space.md`.
    async fn link(&self) -> anyhow::Result<Runner> {
        let held = {
            let mut held = self.link.lock().unwrap_or_else(|e| e.into_inner());
            match held.as_ref().filter(|runner| runner.connected()) {
                Some(runner) => return Ok(runner.clone()),
                None => held.take(),
            }
        };
        drop(held);
        let command: Command = self.endpoint.runner_command()?;
        let fresh = Runner::spawn(command, self.endpoint.answer_timeout())
            .await
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.describe()))?;
        *self.link.lock().unwrap_or_else(|e| e.into_inner()) = Some(fresh.clone());
        Ok(fresh)
    }
}

/// The peers this process has dialled, by the name the operator gave them.
///
/// # Invariant
///
/// One entry per remote for the life of the process, so a reference naming a peer and a
/// tool reading it share one connection. See `docs/notes/server-space.md`.
fn peers() -> &'static Mutex<std::collections::HashMap<String, Space>> {
    static PEERS: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Space>>> =
        std::sync::OnceLock::new();
    PEERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// One program on this machine: no protocol, because there is nobody to speak to.
async fn run_here(
    cwd: &Path,
    program: &str,
    args: &[String],
    stdin: Option<Vec<u8>>,
) -> Result<Run, String> {
    use tokio::io::AsyncWriteExt;
    let mut command = tokio::process::Command::new(program);
    if !cwd.as_os_str().is_empty() {
        command.current_dir(cwd);
    }
    command
        .args(args)
        .stdin(match stdin.is_some() {
            true => std::process::Stdio::piped(),
            false => std::process::Stdio::null(),
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    // Dropped here, so a program reading its standard input sees the end of it.
    if let Some(bytes) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        pipe.write_all(&bytes)
            .await
            .map_err(|e| format!("could not write to {program}: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("waiting for {program}: {e}"))?;
    Ok(Run {
        exit: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}
