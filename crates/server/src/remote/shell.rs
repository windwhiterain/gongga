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
use crate::tools::bash::{
    BackgroundNotifier, BashChunk, BashOutcome, BashStream, background_done_marker,
    background_script,
};

/// The one command that says where a peer's shell is, in its own machine's spelling.
const PWD: &str = "command pwd -W 2>/dev/null || command pwd";

/// Asks the endpoint where it keeps temporary files, without moving its shell.
const TEMP_DIR: &str =
    "( command cd \"${TMPDIR:-/tmp}\" && { command pwd -W 2>/dev/null || command pwd; } )";

/// Why a shell on the endpoint can send no sentinel again.
const ENDED_NO_CONNECTION: &str = "this shell never connected to the endpoint";
const ENDED_QUEUE: &str = "the endpoint's connection ended before the command did";
const ENDED_SHELL: &str = "the shell on the endpoint ended before the command did";

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
    /// Which job number this shell hands out next.
    next_job: u64,
    /// The endpoint's temporary directory, asked once. See `temp_dir`.
    temp_dir: Option<String>,
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
            next_job: 0,
            temp_dir: None,
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
        self.run_tracking_cwd(command, Some(timeout), sink, true)
            .await
    }

    /// Runs one command and waits until it answers, however long that takes.
    ///
    /// # Invariant
    ///
    /// No clock: this returns when both sentinels arrive or the shell is gone, so a
    /// caller with nothing to gain from a partial answer never guesses.
    pub async fn run_until_settled(&mut self, command: &str) -> anyhow::Result<BashOutcome> {
        self.run_tracking_cwd(command, None, None, false).await
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
        self.run_tracking_cwd(command, Some(timeout), None, false)
            .await
    }

    async fn run_tracking_cwd(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
        sink: Option<&mpsc::Sender<BashChunk>>,
        track_cwd: bool,
    ) -> anyhow::Result<BashOutcome> {
        self.write_command(command).await?;
        let pumped = match timeout {
            Some(limit) => tokio::time::timeout(limit, self.pump(sink))
                .await
                .map_err(|_| ()),
            None => Ok(self.pump(sink).await),
        };
        match pumped {
            // The shell can send no sentinel: there is no outcome to report.
            Ok(Err(ended)) => Err(anyhow::Error::new(crate::tools::bash::ShellEnded(
                ended.to_string(),
            ))),
            Ok(Ok(())) => {
                // Taken before `pwd`, which resets both states for its own command.
                let stdout = std::mem::take(&mut self.stdout_state.text);
                let stderr = std::mem::take(&mut self.stderr_state.text);
                let code = self.stdout_state.code.or(self.stderr_state.code);
                self.stdout_state = DrainState::default();
                self.stderr_state = DrainState::default();
                if track_cwd && self.usable() {
                    let asked = match timeout {
                        Some(limit) => tokio::time::timeout(limit, self.pwd())
                            .await
                            .map_err(|_| ()),
                        None => Ok(self.pwd().await),
                    };
                    match asked {
                        Ok(Ok(pwd)) => self.set_cwd(PathBuf::from(pwd)),
                        Ok(Err(_)) => {}
                        Err(()) => self.out_of_sync = true,
                    }
                }
                Ok(BashOutcome {
                    exit_code: code,
                    stdout,
                    stderr,
                    still_running: false,
                })
            }
            Err(()) => {
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

    /// Starts a command in this shell without waiting; `notify` hears when it ends.
    ///
    /// # Invariant
    ///
    /// The job runs in this shell, on the peer: launching it here would run the
    /// model's command on the wrong machine. Its end is an event, never a clock --
    /// the wrapper's sentinel in the endpoint's file, or this shell's channel ending.
    /// See `docs/notes/server-remote.md`.
    pub async fn run_background(
        &mut self,
        command: &str,
        notify: Option<BackgroundNotifier>,
    ) -> anyhow::Result<u64> {
        self.next_job += 1;
        let job = self.next_job;
        let dir = self.temp_dir().await?;
        let out_path = format!("{dir}/gongga-bg-{}-{job}.out", self.marker);
        let done_marker = background_done_marker(job);
        // No pid file: a peer has no question "is this wrapper alive", and its shell's
        // end is the event that answers it.
        let script = background_script(command, &out_path, None, &done_marker);
        let control = {
            let shell = self.connect().await?;
            shell.write(script.as_bytes()).await?;
            shell.control()
        };
        self.jobs.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(tail_remote_background(
            control,
            out_path,
            done_marker,
            job,
            self.pending.clone(),
            self.jobs.clone(),
            notify,
        ));
        Ok(job)
    }

    /// Where the endpoint keeps temporary files, in its own machine's spelling.
    ///
    /// # Invariant
    ///
    /// Asked once per shell and kept: the shell's own `cd` must not move, so the
    /// question is a subshell, and the answer is the path this side stores and hands
    /// back to a file read.
    async fn temp_dir(&mut self) -> anyhow::Result<String> {
        if let Some(dir) = &self.temp_dir {
            return Ok(dir.clone());
        }
        let out = self.run_until_settled(TEMP_DIR).await?;
        let raw = out.stdout.trim().to_string();
        if raw.is_empty() {
            anyhow::bail!("the endpoint did not say where its temporary directory is");
        }
        let dir = self.canonical(&raw).await?;
        self.temp_dir = Some(dir.clone());
        Ok(dir)
    }

    /// Reads until the command that outlived its deadline finishes.
    pub async fn finish_outstanding(&mut self) -> (String, String, Option<i32>) {
        // No deadline: this ends when the sentinel arrives or the shell does.
        if self.pump(None).await.is_err() {
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

    /// The endpoint's own spelling of `path`, asked of the peer that owns it.
    ///
    /// # Invariant
    ///
    /// Every path this side keeps comes back through here: a shell's `pwd` answers a
    /// spelling of its own (`/c/...` under Git Bash), which is not one this machine
    /// can open again, so a reported directory that were stored as said would break
    /// the next `open` on a session whose shell has since been replaced.
    pub async fn canonical(&mut self, path: &str) -> anyhow::Result<String> {
        self.connect().await?;
        let Some((runner, _)) = &self.connection else {
            anyhow::bail!("the endpoint's connection went away");
        };
        let file = runner.stat(path).await?;
        if file.path.trim().is_empty() {
            anyhow::bail!("the endpoint named no path for {path:?}");
        }
        Ok(file.path)
    }

    /// Asks the endpoint where the shell is; an endpoint-local path comes back.
    ///
    /// # Invariant
    ///
    /// `-W` asks for the native spelling, and `canonical` makes either answer one
    /// this side can store.
    pub async fn pwd(&mut self) -> anyhow::Result<String> {
        self.write_command(PWD).await?;
        // A shell that ended cannot answer; that is the caller's to hear.
        if let Err(ended) = self.pump(None).await {
            anyhow::bail!("{ended}");
        }
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
        // The shell's answer is its own spelling; the endpoint's is the one kept.
        let line = self.canonical(&line).await.unwrap_or(line);
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

    /// Drains both streams to their sentinels.
    ///
    /// # Invariant
    ///
    /// Ok means both sentinels arrived. Err means this shell can send no sentinel
    /// ever again -- its queue ended, or the peer said it ended -- which is a death
    /// to report, not a wait to keep taking. Nothing here decides either by a clock.
    async fn pump(&mut self, sink: Option<&mpsc::Sender<BashChunk>>) -> anyhow::Result<()> {
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
            anyhow::bail!("{}", ENDED_NO_CONNECTION);
        };
        while !stdout_state.settled || !stderr_state.settled {
            let event = match shell.next_event().await {
                Some(event) => event,
                None => {
                    // The peer's channel ended: no sentinel can follow.
                    *out_of_sync = true;
                    anyhow::bail!("{}", ENDED_QUEUE);
                }
            };
            match event {
                Event::Out { stream, data, .. } => {
                    let Some(bytes) = event_bytes(&data) else {
                        *out_of_sync = true;
                        anyhow::bail!("the endpoint sent output this build cannot read");
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
                    anyhow::bail!("{}", ENDED_SHELL);
                }
                Event::Hello { .. } => {}
            }
        }
        Ok(())
    }
}

/// Follows one background job's output file on the endpoint.
///
/// # Invariant
///
/// The job ends on one of two events, never on a clock: its wrapper writes the
/// sentinel into the file, or this shell's channel ends -- the job is a child of that
/// shell, so a dead shell is a dead job. The file is removed through the shell at the
/// end, because a peer has no file to delete from here.
async fn tail_remote_background(
    control: ShellControl,
    path: String,
    done_marker: String,
    job: u64,
    pending: Arc<StdMutex<String>>,
    jobs: Arc<AtomicUsize>,
    notify: Option<BackgroundNotifier>,
) {
    let started = std::time::Instant::now();
    let mut offset = 0u64;
    let mut missing = 0u32;
    let mut carry = Vec::new();
    let mut settled = false;
    let mut ended = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(crate::tools::bash::POLL) => {}
            _ = control.ended() => ended = true,
        }
        // Read after the wait either way, so a wrapper that just exited is not missed.
        match control.read(&path, offset).await {
            Ok(chunk) => {
                missing = 0;
                if !chunk.data.is_empty() {
                    settled = crate::tools::bash::push_background_chunk(
                        &mut carry,
                        &chunk.data,
                        &done_marker,
                        &pending,
                    );
                    offset = chunk.next;
                }
            }
            Err(_) => missing += 1,
        }
        if settled || ended {
            break;
        }
        // The shell is alive and the file never appeared: the wrapper did not run.
        if missing > crate::tools::bash::MISSING_FILE_POLLS {
            break;
        }
    }
    if !settled && !carry.is_empty() {
        crate::tools::bash::collect(&pending, &String::from_utf8_lossy(&carry));
    }
    // Ended before anyone is told, so no notice outruns `check_bash`.
    jobs.fetch_sub(1, Ordering::SeqCst);
    if settled || ended {
        if let Some(notify) = notify {
            let code = if settled {
                crate::tools::bash::exit_code(&carry)
            } else {
                None
            };
            notify(crate::tools::bash::BackgroundEnd {
                job,
                code,
                ran: started.elapsed(),
            });
        }
    }
    // Removed by the shell that made it; a shell that is gone takes it with its tree.
    let cleanup = format!(
        "command rm -f {} >/dev/null 2>&1 &\n",
        crate::tools::bash::shell_quote(&path)
    );
    let _ = control.write(cleanup.as_bytes()).await;
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
