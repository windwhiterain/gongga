//! A shell on the endpoint, driven with the same sentinel protocol as a local one.
//!
//! See `docs/notes/server-remote.md`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::remote::endpoint::Endpoint;
use crate::remote::protocol::Event;
use crate::remote::transport::{CloseReport, Runner, ShellControl, ShellHandle};
use crate::tools::bash::{BashChunk, BashOutcome, BashStream};

/// How long a caller waits for another one's close to report its outcome.
const CLOSE_WAIT: Duration = Duration::from_secs(30);

/// Whether a shell may still be opened, and the one place its end is decided.
///
/// # Invariant
///
/// Retirement and publication are one state: a connection that arrives after its
/// shell was retired is handed back to be closed at once, and every caller of
/// `close` observes the same single outcome. See `docs/notes/server-remote.md`.
pub struct Door {
    state: StdMutex<DoorState>,
    /// Wakes a caller waiting for the one outcome.
    settled: tokio::sync::Notify,
}

/// Where a shell's end has got to.
enum DoorState {
    /// Nothing has been opened on the peer.
    Unopened,
    /// A connection attempt is in flight.
    Opening,
    /// Live on the peer, and addressable without the shell's own lock.
    Connected(ShellControl),
    /// Retired while it was being opened: the opener closes what it gets.
    Orphaned,
    /// One caller is closing it.
    Closing,
    /// The outcome, which every later caller is given.
    Closed(CloseReport),
}

/// What an opener may do with the door as it is.
#[derive(Debug, PartialEq, Eq)]
pub enum Begin {
    /// Nothing was opened yet and nothing has retired this shell.
    Proceed,
    /// The shell was retired: opening it would leave a peer-side shell unowned.
    Refused,
}

impl Default for Door {
    fn default() -> Self {
        Self {
            state: StdMutex::new(DoorState::Unopened),
            settled: tokio::sync::Notify::new(),
        }
    }
}

impl Door {
    fn lock(&self) -> std::sync::MutexGuard<'_, DoorState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claims the right to connect, unless this shell has been retired.
    pub fn begin_open(&self) -> Begin {
        let mut state = self.lock();
        match &*state {
            DoorState::Unopened => {
                *state = DoorState::Opening;
                Begin::Proceed
            }
            _ => Begin::Refused,
        }
    }

    /// A connection that did not happen: nothing is on the peer, so nothing is owed.
    pub fn open_failed(&self) {
        let mut state = self.lock();
        match &*state {
            // Retired meanwhile, and nothing was opened: nothing is owed.
            DoorState::Orphaned => {
                *state = DoorState::Closed(CloseReport {
                    confirmed: true,
                    unknown: None,
                });
                drop(state);
                self.settled.notify_waiters();
            }
            _ => *state = DoorState::Unopened,
        }
    }

    /// Publishes a connection; `Some` back means it must be closed by the opener.
    pub fn opened(&self, control: ShellControl) -> Option<ShellControl> {
        let mut state = self.lock();
        match &*state {
            DoorState::Opening => {
                *state = DoorState::Connected(control);
                None
            }
            _ => Some(control),
        }
    }

    /// Records the outcome an opener reached for a connection it had to close.
    pub fn settle(&self, report: CloseReport) {
        let mut state = self.lock();
        if matches!(*state, DoorState::Closing | DoorState::Orphaned) {
            *state = DoorState::Closed(report);
            self.settled.notify_waiters();
        }
    }

    /// Whether this shell still has something that nothing has ended.
    pub fn needs_end(&self) -> bool {
        matches!(
            &*self.lock(),
            DoorState::Unopened | DoorState::Opening | DoorState::Connected(_)
        )
    }

    /// The published control, for a caller that only wants to know it is there.
    pub fn control(&self) -> Option<ShellControl> {
        match &*self.lock() {
            DoorState::Connected(control) => Some(control.clone()),
            _ => None,
        }
    }

    /// Ends the shell once, whichever callers ask: all of them are told the outcome.
    ///
    /// # Invariant
    ///
    /// Only a shell that never opened anything is reported confirmed without the
    /// endpoint saying so; anything else waits for the one close.
    pub async fn close(&self) -> CloseReport {
        loop {
            // Taken before the state is read: a settle between the two still wakes this.
            let settled = self.settled.notified();
            let action = {
                let mut state = self.lock();
                match &*state {
                    DoorState::Unopened => {
                        // Nothing was opened on the peer, so nothing is left there.
                        let report = CloseReport {
                            confirmed: true,
                            unknown: None,
                        };
                        *state = DoorState::Closed(report.clone());
                        return report;
                    }
                    DoorState::Opening => {
                        *state = DoorState::Orphaned;
                        Action::Wait
                    }
                    DoorState::Orphaned | DoorState::Closing => Action::Wait,
                    DoorState::Connected(_) => {
                        let DoorState::Connected(control) = &*state else {
                            unreachable!("just matched");
                        };
                        let control = control.clone();
                        *state = DoorState::Closing;
                        Action::Close(control)
                    }
                    DoorState::Closed(report) => return report.clone(),
                }
            };
            match action {
                Action::Close(control) => {
                    let report = control.close().await;
                    self.settle(report.clone());
                    return report;
                }
                Action::Wait => {
                    if tokio::time::timeout(CLOSE_WAIT, settled).await.is_err() {
                        return CloseReport {
                            confirmed: false,
                            unknown: Some(
                                "another caller's end of this shell did not report in time"
                                    .to_string(),
                            ),
                        };
                    }
                }
            }
        }
    }
}

/// What a caller of `close` does next.
enum Action {
    Close(ShellControl),
    Wait,
}

/// One pipe's resumable state, kept across a wait that ended early.
#[derive(Default)]
struct DrainState {
    text: String,
    /// A line read in progress; a cancelled wait keeps it.
    buf: Vec<u8>,
    code: Option<i32>,
    settled: bool,
}

/// A shell this server does not own: the peer does.
pub struct RemoteShell {
    endpoint: Endpoint,
    /// `None` until a connection says where the endpoint put it.
    cwd: Option<PathBuf>,
    /// A view of `cwd` readable while this shell's own lock is held.
    cwd_cell: Arc<StdMutex<PathBuf>>,
    marker: String,
    /// `None` until the first command: connecting is not done under a live lock.
    connection: Option<(Runner, ShellHandle)>,
    /// Shared with the set that owns this shell, which is what a teardown reads.
    door: Arc<Door>,
    stdout_state: DrainState,
    stderr_state: DrainState,
    out_of_sync: bool,
    pending: Arc<StdMutex<String>>,
    jobs: Arc<AtomicUsize>,
}

impl RemoteShell {
    /// A shell that will be connected on its first command.
    ///
    /// # Invariant
    ///
    /// Nothing here reaches the network: this is built while the session manager's
    /// live lock is held, and a connection must not be.
    pub fn new(endpoint: Endpoint, cwd: Option<PathBuf>, door: Arc<Door>) -> Self {
        Self {
            endpoint,
            door,
            cwd_cell: Arc::new(StdMutex::new(cwd.clone().unwrap_or_default())),
            cwd,
            marker: format!(
                "__GONGGA_{}__",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ),
            connection: None,
            stdout_state: DrainState::default(),
            stderr_state: DrainState::default(),
            out_of_sync: false,
            pending: Arc::new(StdMutex::new(String::new())),
            jobs: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Where the shell is, or empty until its first connection says.
    pub fn cwd(&self) -> &std::path::Path {
        self.cwd.as_deref().unwrap_or(std::path::Path::new(""))
    }

    pub fn cwd_cell(&self) -> Arc<StdMutex<PathBuf>> {
        self.cwd_cell.clone()
    }

    fn set_cwd(&mut self, path: PathBuf) {
        self.cwd = Some(path.clone());
        if let Ok(mut guard) = self.cwd_cell.lock() {
            *guard = path;
        }
    }

    pub fn usable(&self) -> bool {
        !self.out_of_sync
    }

    pub fn pending_len(&self) -> usize {
        self.pending.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn background_jobs(&self) -> usize {
        self.jobs.load(Ordering::SeqCst)
    }

    pub fn take_pending(&mut self) -> String {
        self.pending
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    pub fn take_partial_output(&mut self) -> (String, String) {
        (
            std::mem::take(&mut self.stdout_state.text),
            std::mem::take(&mut self.stderr_state.text),
        )
    }

    /// What the peer says its own pid for this shell is; 0 before it is connected.
    pub fn pid(&self) -> u32 {
        self.connection
            .as_ref()
            .map(|(_, shell)| shell.pid)
            .unwrap_or(0)
    }

    /// Where the shell was rooted, as the endpoint canonicalised it, once connected.
    pub fn endpoint_cwd(&self) -> Option<String> {
        self.connection.as_ref().map(|(_, shell)| shell.cwd.clone())
    }

    /// The connection, opened if this is its first command.
    async fn connect(&mut self) -> anyhow::Result<&mut ShellHandle> {
        if self.connection.is_none() {
            let _ = self.door.begin_open();
            // No directory asked for means the endpoint's own: a path from this
            // machine would name one the peer may not have.
            let asked = self.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
            let shell = match self.endpoint.connect(asked.as_deref()).await {
                Ok(shell) => shell,
                Err(e) => {
                    self.door.open_failed();
                    return Err(e);
                }
            };
            self.set_cwd(PathBuf::from(&shell.cwd));
            let runner = Runner::of(&shell);
            if let Some(control) = self.door.opened(shell.control()) {
                // Retired while opening: closed here, and the report is handed on.
                let report = control.close().await;
                self.door.settle(report);
                anyhow::bail!("this shell was discarded while it was starting");
            }
            self.connection = Some((runner, shell));
        }
        Ok(&mut self.connection.as_mut().expect("just connected").1)
    }

    /// Runs one command, streaming to `sink`; the deadline covers waiting only.
    pub async fn run(
        &mut self,
        command: &str,
        timeout: Duration,
        sink: Option<&mpsc::Sender<BashChunk>>,
    ) -> anyhow::Result<BashOutcome> {
        self.run_tracking_cwd(command, timeout, sink, true).await
    }

    /// Runs one command without the `pwd` that keeps the directory in step.
    ///
    /// # Invariant
    ///
    /// Only for a caller that reads nothing about the shell's directory afterwards:
    /// the `pwd` is a whole extra round trip, and a directory listing costs half its
    /// wall time paying for an answer nobody reads. See `docs/notes/server-remote.md`.
    pub async fn run_without_cwd(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> anyhow::Result<BashOutcome> {
        self.run_tracking_cwd(command, timeout, None, false).await
    }

    async fn run_tracking_cwd(
        &mut self,
        command: &str,
        timeout: Duration,
        sink: Option<&mpsc::Sender<BashChunk>>,
        track_cwd: bool,
    ) -> anyhow::Result<BashOutcome> {
        self.write_command(command).await?;
        let pumped = tokio::time::timeout(timeout, self.pump(sink)).await;
        match pumped {
            Ok(()) => {
                // Taken before `pwd`, which resets both states for its own command.
                let stdout = std::mem::take(&mut self.stdout_state.text);
                let stderr = std::mem::take(&mut self.stderr_state.text);
                let code = self.stdout_state.code.or(self.stderr_state.code);
                self.stdout_state = DrainState::default();
                self.stderr_state = DrainState::default();
                if track_cwd && self.usable() {
                    match tokio::time::timeout(timeout, self.pwd()).await {
                        Ok(Ok(pwd)) => self.set_cwd(PathBuf::from(pwd)),
                        Ok(Err(_)) => {}
                        Err(_) => self.out_of_sync = true,
                    }
                }
                Ok(BashOutcome {
                    exit_code: code,
                    stdout,
                    stderr,
                    still_running: false,
                })
            }
            Err(_) => {
                let (stdout, stderr) = self.take_partial_output();
                Ok(BashOutcome {
                    exit_code: None,
                    stdout,
                    stderr,
                    still_running: true,
                })
            }
        }
    }

    /// Refused, never launched here.
    ///
    /// # Invariant
    ///
    /// A background job needs a scratch file and a completion watcher this server can
    /// read; on a peer neither exists yet, and launching one locally would run the
    /// model's command on the wrong machine. See `docs/notes/server-remote.md`.
    pub async fn run_background(
        &mut self,
        _command: &str,
        _notify: Option<crate::tools::bash::BackgroundNotifier>,
    ) -> anyhow::Result<u64> {
        anyhow::bail!(
            "`background` is not available on a remote endpoint in this build: the job's              output and completion are watched this machine's way, and the shell is not on              this machine. Run it in the foreground, or start it with a `&` and a log file              on the endpoint."
        )
    }

    /// Reads until the command that outlived its deadline finishes.
    pub async fn finish_outstanding(&mut self) -> (String, String, Option<i32>) {
        let settled = tokio::time::timeout(Duration::from_secs(600), self.pump(None)).await;
        if settled.is_err() {
            self.out_of_sync = true;
        }
        let code = self.stdout_state.code.or(self.stderr_state.code);
        let stdout = std::mem::take(&mut self.stdout_state.text);
        let stderr = std::mem::take(&mut self.stderr_state.text);
        self.stdout_state = DrainState::default();
        self.stderr_state = DrainState::default();
        (stdout, stderr, code)
    }

    /// Ends the shell on the peer, with what the endpoint could prove.
    pub async fn kill(&mut self) -> CloseReport {
        self.out_of_sync = true;
        let report = self.door.close().await;
        self.connection = None;
        report
    }

    /// Asks the endpoint where the shell is; an endpoint-local path comes back.
    pub async fn pwd(&mut self) -> anyhow::Result<String> {
        self.write_command("command pwd").await?;
        self.pump(None).await;
        if !self.stdout_state.settled || !self.stderr_state.settled {
            self.out_of_sync = true;
            anyhow::bail!("could not read the shell's working directory on the endpoint");
        }
        let out = std::mem::take(&mut self.stdout_state.text);
        self.stdout_state = DrainState::default();
        self.stderr_state = DrainState::default();
        let line = out.lines().next().unwrap_or_default().trim().to_string();
        if line.is_empty() {
            anyhow::bail!("could not read the shell's working directory on the endpoint");
        }
        self.set_cwd(PathBuf::from(&line));
        Ok(line)
    }

    async fn write_command(&mut self, command: &str) -> anyhow::Result<()> {
        let marker = self.marker.clone();
        let script = format!(
            "{command}\n__gongga_st=$?\ncommand printf '\\n{marker}%d\\n' \"$__gongga_st\"\ncommand printf '\\n{marker}%d\\n' \"$__gongga_st\" >&2\n"
        );
        // Each pipe settles on its own, so a resuming wait must not read the old sentinel.
        self.stdout_state.settled = false;
        self.stderr_state.settled = false;
        let shell = self.connect().await?;
        shell.write(script.as_bytes()).await
    }

    /// Drains both streams to their sentinels; a timeout leaves the state resumable.
    async fn pump(&mut self, sink: Option<&mpsc::Sender<BashChunk>>) {
        let marker = self.marker.clone();
        let Self {
            connection,
            stdout_state,
            stderr_state,
            out_of_sync,
            ..
        } = self;
        let Some((_, shell)) = connection else {
            *out_of_sync = true;
            return;
        };
        while !stdout_state.settled || !stderr_state.settled {
            let event = match shell.next_event().await {
                Some(event) => event,
                None => {
                    // The peer's channel ended: no sentinel can follow.
                    *out_of_sync = true;
                    return;
                }
            };
            match event {
                Event::Out { stream, data, .. } => {
                    let Some(bytes) = event_bytes(&data) else {
                        *out_of_sync = true;
                        return;
                    };
                    let state = match stream {
                        crate::remote::protocol::Stream::Stdout => &mut *stdout_state,
                        crate::remote::protocol::Stream::Stderr => &mut *stderr_state,
                    };
                    let stream = match stream {
                        crate::remote::protocol::Stream::Stdout => BashStream::Stdout,
                        crate::remote::protocol::Stream::Stderr => BashStream::Stderr,
                    };
                    state.buf.extend_from_slice(&bytes);
                    drain_lines(state, &marker, sink, stream).await;
                }
                Event::End { .. } => {
                    // The shell ended: what is left is not a command's output.
                    *out_of_sync = true;
                    return;
                }
                Event::Hello { .. } => {}
            }
        }
    }
}

/// The bytes one `out` event carried.
fn event_bytes(data: &str) -> Option<Vec<u8>> {
    crate::base64::decode(data)
}

/// Consumes whole lines from a pipe's buffer, up to and including the sentinel.
async fn drain_lines(
    state: &mut DrainState,
    marker: &str,
    sink: Option<&mpsc::Sender<BashChunk>>,
    stream: BashStream,
) {
    if state.settled {
        return;
    }
    while let Some(pos) = state.buf.iter().position(|b| *b == b'\n') {
        let mut line: Vec<u8> = state.buf.drain(..=pos).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        if let Some(rest) = text.strip_prefix(marker) {
            state.buf.clear();
            state.code = rest.trim().parse::<i32>().ok();
            state.settled = true;
            return;
        }
        if !state.text.is_empty() {
            state.text.push('\n');
        }
        state.text.push_str(&text);
        if let Some(sink) = sink {
            let mut chunk = text;
            chunk.push('\n');
            let _ = sink
                .send(BashChunk {
                    stream,
                    text: chunk,
                })
                .await;
        }
    }
}
