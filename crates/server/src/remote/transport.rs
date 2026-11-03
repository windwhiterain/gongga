//! The client half: one child process, and requests paired with their answers by id.
//!
//! See `docs/notes/server-remote.md`.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::remote::protocol::{Event, Frame, Op, PROTOCOL, Reply, Request};

/// Frames in flight per shell, and requests waiting for the writer.
///
/// See `docs/notes/server-remote.md`.
const WINDOW: usize = 8;

/// How long a runner has to greet, so a wrong program is refused, not waited on.
const GREETING: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a dropped transport waits for its runner to end its own shells.
const DISCONNECT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// The peer speaks another protocol: the two ends cannot talk.
///
/// # Invariant
///
/// Only a distribution on the peer can change that, and the reader is told which
/// command does it. See `docs/notes/server-remote.md`.
#[derive(Debug, thiserror::Error)]
#[error("the endpoint's runner speaks protocol {peer}, and this server speaks protocol {server}")]
pub struct ProtocolMismatch {
    /// The number the peer's runner greeted with.
    pub peer: u32,
    /// The number this server speaks.
    pub server: u32,
}

/// One shell's output in flight, and the flow control that keeps it bounded.
///
/// # Invariant
///
/// Reading this queue never blocks the connection's reader: a shell whose client
/// is behind is paused on the peer instead, so another shell's output and every
/// answer keep flowing. See `docs/notes/server-remote.md`.
#[derive(Default)]
struct ShellQueue {
    events: StdMutex<ShellQueueState>,
    /// Wakes a reader waiting for the next event.
    ready: tokio::sync::Notify,
    /// Wakes the one task that keeps the peer's pause state in step with this
    /// queue. See `reconcile_flow`.
    flow: tokio::sync::Notify,
}

#[derive(Default)]
struct ShellQueueState {
    frames: std::collections::VecDeque<Event>,
    /// Set when the connection ended: a reader then learns it is over.
    ended: bool,
}

impl ShellQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Adds one frame.
    ///
    /// # Invariant
    ///
    /// Nothing is dropped: a frame over the window is kept, and the peer is paused
    /// from the one place that sends flow ops.
    fn push(&self, event: Event) {
        let mut state = self.events.lock().unwrap_or_else(|e| e.into_inner());
        state.frames.push_back(event);
        self.ready.notify_waiters();
        self.flow.notify_one();
    }

    /// Frames held here now.
    fn len(&self) -> usize {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frames
            .len()
    }

    fn ended(&self) -> bool {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).ended
    }

    /// Resolves once this queue has ended.
    async fn wait_ended(&self) {
        loop {
            let notified = self.ready.notified();
            if self.ended() {
                return;
            }
            notified.await;
        }
    }

    /// The next frame, waiting for one if the queue is empty; `None` at the end.
    async fn next(&self) -> Option<Event> {
        loop {
            let notified = self.ready.notified();
            {
                let mut state = self.events.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(event) = state.frames.pop_front() {
                    drop(state);
                    self.flow.notify_one();
                    return Some(event);
                }
                if state.ended {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Ends the queue: a reader learns the connection is over rather than waiting.
    fn end(&self) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).ended = true;
        self.ready.notify_waiters();
        self.flow.notify_one();
    }
}

/// How long a reconciler waits before trying a flow op the peer did not take.
const FLOW_RETRY: std::time::Duration = std::time::Duration::from_millis(200);

/// Keeps one shell's pause state on the peer equal to what this side wants.
///
/// # Invariant
///
/// Exactly one task sends this shell's `Pause` and `Resume`, and it sends what the
/// queue says *now*, never an edge decided earlier. Two tasks deciding would race:
/// ops from independent tasks arrive in no fixed order, and the loser's op lands
/// last and stays in force, leaving the peer paused with nothing left to resume it.
fn reconcile_flow(inner: std::sync::Weak<Inner>, shell: u64, queue: Arc<ShellQueue>) {
    tokio::spawn(async move {
        let mut paused = false;
        loop {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            // Hysteresis: paused until drained, paused again only at the window.
            let want = if paused {
                queue.len() > 0
            } else {
                queue.len() >= WINDOW
            };
            if want != paused {
                let op = if want {
                    Op::Pause { shell }
                } else {
                    Op::Resume { shell }
                };
                let runner = Runner { inner };
                // Answered, not merely written: what the peer holds is what it said.
                if let Ok(Ok(_)) = tokio::time::timeout(FLOW_RETRY, runner.request(op)).await {
                    paused = want;
                }
            }
            if queue.ended() {
                return;
            }
            tokio::select! {
                _ = queue.flow.notified() => {}
                _ = tokio::time::sleep(FLOW_RETRY) => {}
            }
        }
    });
}

/// The endpoint answered, and its answer was no: this command did not run there.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the endpoint refused: {}", self.0)
    }
}

impl std::error::Error for Refused {}

/// The connection itself is gone; what ran is not knowable from here.
#[derive(Debug)]
pub struct Lost(pub String);

impl std::fmt::Display for Lost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the endpoint's connection is gone: {}", self.0)
    }
}

impl std::error::Error for Lost {}

/// One endpoint's runner process, and every request in flight on it.
#[derive(Clone)]
pub struct Runner {
    inner: Arc<Inner>,
}

struct Inner {
    /// Requests sent and not yet answered, by id.
    pending: StdMutex<HashMap<u64, oneshot::Sender<Reply>>>,
    /// One channel per open shell: its own output, in order, bounded by `WINDOW`.
    routes: StdMutex<HashMap<u64, Arc<ShellQueue>>>,
    writer: mpsc::Sender<String>,
    /// Why the connection ended, once it has.
    closed: StdMutex<Option<String>>,
    next_id: AtomicU64,
    next_shell: AtomicU64,
    /// The runner's own pid on the peer, as its greeting said.
    pid: AtomicU32,
    /// Cancelled by the last owner, which is what ends the child process.
    shutdown: CancellationToken,
}

impl Inner {
    /// Records the end of the connection; every waiter then stops instead of hanging.
    /// # Invariant
    ///
    /// Ends the shell queues as well as the request waiters: a shell's reader waits
    /// on its own queue, not on a request, so clearing `pending` alone left it
    /// waiting on a connection that would never speak again. Measured: a listing
    /// whose transport died hung for over five minutes with no error.
    fn close(&self, reason: impl Into<String>) {
        let reason = reason.into();
        let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        if closed.is_some() {
            return;
        }
        *closed = Some(reason);
        // Dropping the senders is what wakes the waiters; each reads `closed` for why.
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Every shell reader learns the connection ended, in one pass.
        let routes: Vec<Arc<ShellQueue>> = {
            let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
            routes.drain().map(|(_, queue)| queue).collect()
        };
        for queue in routes {
            queue.end();
        }
    }

    fn lost(&self) -> anyhow::Error {
        let reason = self
            .closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| "the runner stopped without saying why".to_string());
        anyhow::Error::new(Lost(reason))
    }

    fn check_open(&self) -> anyhow::Result<()> {
        match self
            .closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            true => Err(self.lost()),
            false => Ok(()),
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Runner {
    /// Starts `command` and waits for its greeting, so a peer this client cannot
    /// speak to is refused first.
    pub async fn spawn(mut command: Command) -> anyhow::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("the runner command has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("the runner command has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("the runner command has no stderr"))?;

        let (writer, writing) = mpsc::channel::<String>(WINDOW);
        let (hello_sender, hello) = oneshot::channel::<Event>();
        let shutdown = CancellationToken::new();
        let inner = Arc::new(Inner {
            pending: StdMutex::new(HashMap::new()),
            routes: StdMutex::new(HashMap::new()),
            writer,
            closed: StdMutex::new(None),
            next_id: AtomicU64::new(1),
            next_shell: AtomicU64::new(1),
            pid: AtomicU32::new(0),
            shutdown: shutdown.clone(),
        });

        tokio::spawn(write_loop(writing, stdin, Arc::downgrade(&inner)));
        tokio::spawn(read_loop(
            BufReader::new(stdout),
            Arc::downgrade(&inner),
            hello_sender,
        ));
        tokio::spawn(watch_stderr(BufReader::new(stderr), Arc::downgrade(&inner)));
        tokio::spawn(reap(child, Arc::downgrade(&inner), shutdown));

        // A peer that neither greets nor exits would otherwise be waited on forever.
        let first = match tokio::time::timeout(GREETING, hello).await {
            Ok(Ok(first)) => first,
            Ok(Err(_)) => {
                let reason = inner
                    .closed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_else(|| "it said nothing".to_string());
                anyhow::bail!("the endpoint's runner did not answer: {reason}");
            }
            Err(_) => anyhow::bail!(
                "the endpoint's runner did not greet within {GREETING:?}; is that program \
                 really the runner?"
            ),
        };
        let Event::Hello { protocol, pid } = first else {
            anyhow::bail!("the endpoint's runner greeted with something else");
        };
        if protocol != PROTOCOL {
            return Err(ProtocolMismatch {
                peer: protocol,
                server: PROTOCOL,
            }
            .into());
        }
        inner.pid.store(pid, Ordering::SeqCst);
        Ok(Self { inner })
    }

    /// The runner behind one of its own shells, for a caller that only kept the shell.
    pub fn of(shell: &ShellHandle) -> Runner {
        shell.runner.clone()
    }

    /// The peer's own pid for the runner process, once it has greeted.
    pub fn pid(&self) -> u32 {
        self.inner.pid.load(Ordering::SeqCst)
    }

    /// One request, one answer; ids are what let a signal overtake a busy command.
    pub async fn request(&self, op: Op) -> anyhow::Result<Value> {
        self.inner.check_open()?;
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, answer) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, sender);
        let line = serde_json::to_string(&Request::new(id, op))?;
        if self.inner.writer.send(line).await.is_err() {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(self.inner.lost());
        }
        let reply = match answer.await {
            Ok(reply) => reply,
            Err(_) => return Err(self.inner.lost()),
        };
        match (reply.ok, reply.err) {
            (Some(value), _) => Ok(value),
            (None, Some(message)) => Err(anyhow::Error::new(Refused(message))),
            (None, None) => Err(anyhow::anyhow!("the endpoint sent an empty answer")),
        }
    }

    /// Starts a shell on the peer, rooted at an endpoint-local directory.
    pub async fn open(
        &self,
        cwd: Option<&str>,
        program: Option<&str>,
    ) -> anyhow::Result<ShellHandle> {
        self.open_with(cwd, program, None).await
    }

    /// Starts one program on the peer: a shell when `args` is `None`, else exactly
    /// what `args` says.
    pub async fn open_with(
        &self,
        cwd: Option<&str>,
        program: Option<&str>,
        args: Option<&[String]>,
    ) -> anyhow::Result<ShellHandle> {
        let id = self.inner.next_shell.fetch_add(1, Ordering::SeqCst);
        let output = ShellQueue::new();
        self.inner
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, output.clone());
        // Weak, so this task never keeps the connection alive; it ends with the route.
        reconcile_flow(Arc::downgrade(&self.inner), id, output.clone());
        let op = Op::Open {
            shell: id,
            cwd: cwd.map(str::to_string),
            program: program.map(str::to_string),
            args: args.map(<[String]>::to_vec),
        };
        let reply = match self.request(op).await {
            Ok(reply) => reply,
            Err(e) => {
                unroute(self, id);
                return Err(e);
            }
        };
        Ok(ShellHandle {
            runner: self.clone(),
            id,
            pid: reply["pid"].as_u64().unwrap_or_default() as u32,
            cwd: reply["cwd"].as_str().unwrap_or_default().to_string(),
            output,
        })
    }

    /// What the endpoint says is at `path`; the path is the endpoint's, not this machine's.
    pub async fn stat(&self, path: &str) -> anyhow::Result<EndpointFile> {
        let reply = self
            .request(Op::Stat {
                path: path.to_string(),
            })
            .await?;
        Ok(EndpointFile {
            path: reply["path"].as_str().unwrap_or(path).to_string(),
            exists: reply["exists"].as_bool().unwrap_or(false),
            is_file: reply["is_file"].as_bool().unwrap_or(false),
            is_dir: reply["is_dir"].as_bool().unwrap_or(false),
            size: reply["size"].as_u64().unwrap_or(0),
        })
    }

    /// One chunk of an endpoint file, and where the next read continues.
    pub async fn read(&self, path: &str, offset: u64) -> anyhow::Result<FileChunk> {
        let reply = self
            .request(Op::Read {
                path: path.to_string(),
                offset,
            })
            .await?;
        let data = reply["data"]
            .as_str()
            .and_then(crate::base64::decode)
            .ok_or_else(|| anyhow::anyhow!("the endpoint's file chunk was not base64"))?;
        Ok(FileChunk {
            data,
            next: reply["offset"].as_u64().unwrap_or(offset),
            eof: reply["eof"].as_bool().unwrap_or(false),
        })
    }

    /// The names in one endpoint directory.
    pub async fn list(&self, path: &str) -> anyhow::Result<Vec<DirEntry>> {
        let reply = self
            .request(Op::List {
                path: path.to_string(),
            })
            .await?;
        Ok(reply["entries"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| DirEntry {
                        name: row["name"].as_str().unwrap_or_default().to_string(),
                        is_dir: row["is_dir"].as_bool().unwrap_or(false),
                        is_file: row["is_file"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Whether the connection is still there.
    pub fn connected(&self) -> bool {
        self.inner
            .closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
    }
}

/// One entry of an endpoint directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
}

/// What the endpoint says about one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointFile {
    pub path: String,
    pub exists: bool,
    pub is_file: bool,
    pub is_dir: bool,
    pub size: u64,
}

/// One chunk of an endpoint file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunk {
    pub data: Vec<u8>,
    pub next: u64,
    pub eof: bool,
}

/// What a teardown could prove about a shell on the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReport {
    /// The endpoint confirmed the whole group is gone.
    pub confirmed: bool,
    /// Why that is not known, when it is not.
    pub unknown: Option<String>,
}

/// One shell on the peer: its id, its own output, and no promise about its tree.
pub struct ShellHandle {
    runner: Runner,
    id: u64,
    pub pid: u32,
    /// The endpoint-local directory it was rooted at, as the peer spelled it.
    pub cwd: String,
    output: Arc<ShellQueue>,
}

impl ShellHandle {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// One chunk of a file on this shell's own endpoint, read by its runner.
    ///
    /// See `docs/notes/server-remote.md`.
    pub async fn read(&self, path: &str, offset: u64) -> anyhow::Result<FileChunk> {
        self.runner.read(path, offset).await
    }

    /// The next thing the peer said about this shell: its output, or its end.
    ///
    /// # Invariant
    ///
    /// Reading never blocks the connection: this shell is paused on the peer when
    /// its window fills, and resumed as soon as this side catches up.
    pub async fn next_event(&mut self) -> Option<Event> {
        // Popping wakes the flow reconciler; resuming is its decision, not this one.
        self.output.next().await
    }

    /// Bytes for the shell's stdin; the caller owns the command framing.
    pub async fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
        self.runner
            .request(Op::Write {
                shell: self.id,
                data: crate::base64::encode(bytes),
            })
            .await
            .map(|_| ())
    }

    /// A signal for the shell's whole process group, sent independently of any write.
    pub async fn signal(&self, signal: &str) -> anyhow::Result<bool> {
        let reply = self
            .runner
            .request(Op::Signal {
                shell: self.id,
                signal: signal.to_string(),
            })
            .await?;
        Ok(reply["delivered"].as_bool().unwrap_or(false))
    }

    /// This shell, addressable while another owner holds its handle.
    pub fn control(&self) -> ShellControl {
        ShellControl {
            runner: self.runner.clone(),
            id: self.id,
            pid: self.pid,
            output: self.output.clone(),
        }
    }

    /// Ends the shell on the peer and says whether that is confirmed.
    pub async fn close(self) -> CloseReport {
        self.control().close().await
    }

    fn unroute(&self) {
        unroute(&self.runner, self.id);
    }
}

/// One shell on the peer, addressable without the handle that runs its commands.
///
/// # Invariant
///
/// This is how a teardown reaches a shell whose owner is busy: the request goes to
/// the endpoint, and the pid it carries is the peer's -- never signalled here. See
/// `docs/notes/server-remote.md`.
#[derive(Clone)]
pub struct ShellControl {
    runner: Runner,
    id: u64,
    /// The endpoint's own pid, for a report; this machine cannot signal it.
    pid: u32,
    /// The shell's own output queue, for a watcher that must not take its lock.
    output: Arc<ShellQueue>,
}

impl ShellControl {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The endpoint's pid for this shell, which is not a pid on this machine.
    pub fn endpoint_pid(&self) -> u32 {
        self.pid
    }

    /// One chunk of a file on this shell's endpoint, read by the same runner.
    pub async fn read(&self, path: &str, offset: u64) -> anyhow::Result<FileChunk> {
        self.runner.read(path, offset).await
    }

    /// Bytes for the shell's stdin, sent without taking its lock.
    pub async fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
        self.runner
            .request(Op::Write {
                shell: self.id,
                data: crate::base64::encode(bytes),
            })
            .await
            .map(|_| ())
    }

    /// Resolves when this shell is over: its channel ended, or its route went.
    ///
    /// # Invariant
    ///
    /// A watcher learns a shell's end as an event, never by polling a clock: a
    /// background job that writes no sentinel because its shell died is otherwise
    /// indistinguishable from one that is merely slow.
    pub async fn ended(&self) {
        self.output.wait_ended().await
    }

    /// Ends the shell on the peer and says whether that is confirmed.
    ///
    /// # Invariant
    ///
    /// Only the endpoint's own proof counts: a dropped connection, or an endpoint
    /// that answered neither way, leaves this unconfirmed -- which is not the same
    /// as stopped. See `docs/notes/server-remote.md`.
    pub async fn close(&self) -> CloseReport {
        let reply = self.runner.request(Op::Close { shell: self.id }).await;
        // Routed away after the answer: a reader woken before it would miss why.
        unroute(&self.runner, self.id);
        match reply {
            Ok(value) => CloseReport {
                confirmed: value["confirmed"].as_bool().unwrap_or(false),
                unknown: value["unknown"].as_str().map(str::to_string),
            },
            Err(e) => CloseReport {
                confirmed: false,
                unknown: Some(e.to_string()),
            },
        }
    }
}

/// Drops a shell's route, ending it so a reader learns the shell is over.
fn unroute(runner: &Runner, id: u64) {
    let route = runner
        .inner
        .routes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    if let Some(route) = route {
        route.end();
    }
}

impl Drop for ShellHandle {
    /// Drops the local route only: what is left on the peer is not claimed stopped.
    fn drop(&mut self) {
        self.unroute();
    }
}

/// Writes whole lines to the runner's stdin, one request at a time.
async fn write_loop(
    mut queue: mpsc::Receiver<String>,
    mut stdin: tokio::process::ChildStdin,
    inner: std::sync::Weak<Inner>,
) {
    while let Some(line) = queue.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err()
        {
            close(&inner, "the request channel closed");
            return;
        }
        if stdin.flush().await.is_err() {
            close(&inner, "the request channel closed");
            return;
        }
    }
}

/// Reads the runner's frames and hands each to whoever is waiting for it.
async fn read_loop(
    stdout: BufReader<tokio::process::ChildStdout>,
    inner: std::sync::Weak<Inner>,
    hello: oneshot::Sender<Event>,
) {
    let mut hello = Some(hello);
    let mut lines = stdout.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let value: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(e) => {
                        close(&inner, &format!("a frame was not JSON: {e}"));
                        return;
                    }
                };
                let Some(state) = inner.upgrade() else {
                    return;
                };
                match crate::remote::protocol::frame(&value) {
                    Frame::Reply(id, value) => {
                        let sender = state
                            .pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&id);
                        if let Some(sender) = sender {
                            let reply = serde_json::from_value::<Reply>(value)
                                .map_err(|e| format!("a reply this client cannot read: {e}"));
                            let reply = match reply {
                                Ok(reply) => reply,
                                Err(message) => Reply::err(id, message),
                            };
                            let _ = sender.send(reply);
                        }
                        // An answer to a request nobody waits for is a late answer, not an error.
                    }
                    Frame::Event(event) => match event {
                        Event::Hello { .. } if hello.is_some() => {
                            if let Some(sender) = hello.take() {
                                let _ = sender.send(event);
                            }
                        }
                        Event::Out { shell, .. } | Event::End { shell, .. } => {
                            let route = state
                                .routes
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&shell)
                                .cloned();
                            // A route that is gone means nobody owns that shell now.
                            if let Some(route) = route {
                                route.push(event);
                            }
                        }
                        Event::Hello { .. } => {}
                    },
                    Frame::Broken(why) => {
                        close(&inner, &why);
                        return;
                    }
                }
            }
            Ok(None) => {
                close(&inner, "the runner closed its output");
                return;
            }
            Err(e) => {
                close(&inner, &format!("reading the runner's output failed: {e}"));
                return;
            }
        }
    }
}

/// The runner's stderr is the transport's own: `ssh` writes its diagnosis there.
async fn watch_stderr(
    stderr: BufReader<tokio::process::ChildStderr>,
    inner: std::sync::Weak<Inner>,
) {
    let mut lines = stderr.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::warn!(remote = %line, "the endpoint's runner said"),
            Ok(None) => return,
            Err(_) => {
                close(&inner, "reading the runner's stderr failed");
                return;
            }
        }
    }
}

/// Ends the child when the last owner is gone, and records why it ended.
async fn reap(
    mut child: tokio::process::Child,
    inner: std::sync::Weak<Inner>,
    shutdown: CancellationToken,
) {
    tokio::select! {
        status = child.wait() => {
            let reason = match status {
                Ok(status) => format!("the runner process ended ({status})"),
                Err(e) => format!("the runner process could not be waited for: {e}"),
            };
            close(&inner, &reason);
        }
        _ = shutdown.cancelled() => {
            // The runner hears EOF and ends its shells; killing it would abandon them.
            match tokio::time::timeout(DISCONNECT_GRACE, child.wait()).await {
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        grace = ?DISCONNECT_GRACE,
                        "the endpoint's runner did not stop after its channels closed"
                    );
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            }
        }
    }
}

fn close(inner: &std::sync::Weak<Inner>, reason: &str) {
    if let Some(state) = inner.upgrade() {
        state.close(reason);
    }
}
