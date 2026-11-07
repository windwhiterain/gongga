//! The peer side: one stdio program that owns a machine's shells and files.
//!
//! See `docs/notes/server-remote.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::remote::protocol::{Event, Op, PROTOCOL, Reply, Request, Stream};

/// How many bytes one `read` reply carries: one line, however large the file.
const READ_CHUNK: usize = 256 * 1024;
/// TERM is a request: KILL follows only after this grace has passed.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(1);
/// Even KILL is not accepted as proof until the group is observed gone.
const KILL_CONFIRM: Duration = Duration::from_secs(2);
/// How often a teardown re-reads the group while it waits.
const SIGNAL_POLL: Duration = Duration::from_millis(25);
/// How long a `close` waits for its supervisor's report before answering unknown.
const CLOSE_ANSWER: Duration = Duration::from_secs(8);
/// How long `End` waits for a shell's pipes: a daemon holding one must not stop it.
const PIPE_DRAIN: Duration = Duration::from_secs(5);
/// How a peer reaches the runner, unless its target says otherwise.
///
/// # Invariant
///
/// The distribution installs one file, `gongga`, and it dispatches on its first
/// word, so the runner is `gongga runner` and not a program of its own on `PATH`.
/// See `docs/notes/server-remote.md`.
pub const DEFAULT_RUNNER: &str = "gongga runner";

/// The signals a client may name: the set a shell teardown and a cancel need.
const SIGNALS: &[&str] = &["TERM", "KILL", "INT", "HUP", "QUIT"];

/// One shell, and the two streams it writes into.
struct Shell {
    pid: u32,
    /// Every process of this shell, so ending it needs no walk of this machine's table.
    #[cfg(windows)]
    job: Option<crate::tools::job::ProcessJob>,
    /// Whether this shell's pipes are held: the client is behind.
    ///
    /// # Invariant
    ///
    /// A level every pump reads, never a wake one of two can miss.
    paused: tokio::sync::watch::Sender<bool>,
    stdin: Mutex<ChildStdin>,
    /// Continuity for the client: a gap in these numbers is a lost frame.
    stdout_seq: AtomicU64,
    stderr_seq: AtomicU64,
    /// Set before its group is signalled: later work would reach a dying shell.
    /// See `docs/notes/server-remote.md`.
    closing: AtomicBool,
    /// The supervisor owns this shell's `Child`; it is the only thing that ends it.
    closer: mpsc::Sender<oneshot::Sender<CloseReport>>,
}

impl Shell {
    fn next_seq(&self, stream: Stream) -> u64 {
        let counter = match stream {
            Stream::Stdout => &self.stdout_seq,
            Stream::Stderr => &self.stderr_seq,
        };
        counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Refuses work for a shell whose group is being ended.
    fn open_for_work(&self) -> Result<(), String> {
        match self.closing.load(Ordering::SeqCst) {
            true => Err(format!("shell {} is closing", self.pid)),
            false => Ok(()),
        }
    }
}

/// What a teardown could prove about one shell's process group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReport {
    /// The endpoint saw the whole group go.
    pub confirmed: bool,
    /// Why that is not known, when it is not.
    pub unknown: Option<String>,
}

impl CloseReport {
    fn confirmed() -> Self {
        Self {
            confirmed: true,
            unknown: None,
        }
    }

    fn unknown(why: impl Into<String>) -> Self {
        Self {
            confirmed: false,
            unknown: Some(why.into()),
        }
    }

    fn to_json(&self, pid: u32) -> Value {
        json!({
            "confirmed": self.confirmed,
            "unknown": self.unknown,
            "pid": pid,
        })
    }
}

/// Everything the request handlers share.
struct Runner {
    out: Mutex<tokio::io::Stdout>,
    shells: Mutex<HashMap<u64, Arc<Shell>>>,
    /// Cancelled when the client can no longer be written to.
    ///
    /// # Invariant
    ///
    /// Set exactly when a write failed, so no shell keeps running under a
    /// connection that can no longer report what it printed.
    broken: CancellationToken,
}

impl Runner {
    /// Writes one frame, whole, so two tasks never interleave halves of a line.
    ///
    /// # Invariant
    ///
    /// A failed write cancels `broken`: output that cannot be delivered must not
    /// be dropped while the shell in front of it keeps running.
    async fn emit<T: serde::Serialize>(&self, frame: &T) -> bool {
        let Ok(line) = serde_json::to_string(frame) else {
            return true;
        };
        let mut out = self.out.lock().await;
        let written = out.write_all(line.as_bytes()).await.is_ok()
            && out.write_all(b"\n").await.is_ok()
            && out.flush().await.is_ok();
        if !written {
            self.broken.cancel();
        }
        written
    }

    async fn reply(&self, id: u64, result: Result<Value, String>) {
        let reply = match result {
            Ok(value) => Reply::ok(id, value),
            Err(message) => Reply::err(id, message),
        };
        self.emit(&reply).await;
    }

    /// One request on its own task, so a signal or a file read is never queued
    /// behind a shell that is busy.
    async fn serve(self: Arc<Self>, request: Request) {
        let id = request.id;
        if self.broken.is_cancelled() {
            // Answered rather than ignored: a client still reading its side learns why.
            self.reply(
                id,
                Err("the runner can no longer write to this connection".into()),
            )
            .await;
            return;
        }
        let result = match request.op {
            Op::Open {
                shell,
                cwd,
                program,
                args,
            } => self.open(shell, cwd, program, args).await,
            Op::Write { shell, data } => self.write(shell, &data).await,
            Op::Signal { shell, signal } => self.signal(shell, &signal).await,
            Op::Pause { shell } => self.pause(shell, true).await,
            Op::Resume { shell } => self.pause(shell, false).await,
            Op::Close { shell } => self.close(shell).await,
            Op::Stat { path } => stat(&path),
            Op::Read { path, offset } => read(&path, offset).await,
            Op::List { path } => list(&path),
            Op::Space { path } => space(&path),
            Op::Append { path, data } => append(&path, &data),
            Op::Relay { dir, cwd } => relay(&dir, cwd.as_deref()),
        };
        self.reply(id, result).await;
    }

    /// Starts a shell, refusing a directory this machine does not have.
    async fn open(
        self: &Arc<Self>,
        shell: u64,
        cwd: Option<String>,
        program: Option<String>,
        args: Option<Vec<String>>,
    ) -> Result<Value, String> {
        let program = program.unwrap_or_else(|| "bash".to_string());
        let root = match &cwd {
            Some(path) => {
                let path = PathBuf::from(path);
                // The endpoint answers: a path the server has is not a path this peer has.
                std::fs::canonicalize(&path)
                    .map_err(|e| format!("no such directory on the endpoint: {path:?} ({e})"))?
            }
            None => std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?,
        };
        if !root.is_dir() {
            return Err(format!("not a directory on the endpoint: {root:?}"));
        }
        let mut command = Command::new(&program);
        // Shell flags are a shell's: `--norc` to an interpreter is a usage error.
        match &args {
            Some(args) => command.args(args),
            None => command.arg("--norc").arg("--noprofile"),
        };
        command
            .current_dir(&root)
            .env("TERM", "dumb")
            .env("PS1", "")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Its own process group, so one signal reaches the whole command tree.
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        // Suspended until the shell joins its job; nesting works beneath sshd.
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
            // The runner is started by ssh, which has no console to hand down.
            tokio::process::Command::creation_flags(&mut command, console::flags(CREATE_SUSPENDED));
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start {program} on the endpoint: {e}"))?;
        let pid = child.id().unwrap_or_default();
        #[cfg(windows)]
        let job = match crate::tools::job::ProcessJob::adopt(pid)
            .ok_or_else(|| std::io::Error::other("could not put the shell in a job"))
            .and_then(|job| {
                crate::tools::job::ProcessJob::resume_main(pid)?;
                Ok(job)
            }) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(format!("could not isolate shell on the endpoint: {e}"));
            }
        };
        let stdin = child.stdin.take().ok_or("the shell has no stdin")?;
        let stdout = child.stdout.take().ok_or("the shell has no stdout")?;
        let stderr = child.stderr.take().ok_or("the shell has no stderr")?;
        let (closer, closes) = mpsc::channel::<oneshot::Sender<CloseReport>>(4);
        let entry = Arc::new(Shell {
            pid,
            #[cfg(windows)]
            job: Some(job),
            paused: tokio::sync::watch::channel(false).0,
            stdin: Mutex::new(stdin),
            stdout_seq: AtomicU64::new(0),
            stderr_seq: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            closer,
        });
        {
            let mut shells = self.shells.lock().await;
            if shells.contains_key(&shell) {
                return Err(format!("shell {shell} is already open"));
            }
            shells.insert(shell, entry.clone());
        }
        let (stdout_drained, stdout_done) = oneshot::channel();
        let (stderr_drained, stderr_done) = oneshot::channel();
        self.pump(
            shell,
            entry.clone(),
            Stream::Stdout,
            Box::new(stdout),
            stdout_drained,
        );
        self.pump(
            shell,
            entry.clone(),
            Stream::Stderr,
            Box::new(stderr),
            stderr_drained,
        );
        self.supervise(shell, entry, child, closes, vec![stdout_done, stderr_done]);
        Ok(json!({"shell": shell, "pid": pid, "cwd": root}))
    }

    /// Reads one pipe to its end, forwarding every byte as it arrives.
    ///
    /// # Invariant
    ///
    /// The completion signal fires only after the last byte of this pipe has been
    /// handed to `emit`, which is what lets `End` follow every byte.
    fn pump<R>(
        self: &Arc<Self>,
        shell: u64,
        entry: Arc<Shell>,
        stream: Stream,
        pipe: R,
        drained: oneshot::Sender<()>,
    ) where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let runner = self.clone();
        tokio::spawn(async move {
            let mut pipe = pipe;
            // This pump's own view of the pause, created before the first read.
            let mut flow = entry.paused.subscribe();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        // Wait for the level: a resume that arrives before this pump
                        // looks is a value it sees.
                        while *flow.borrow_and_update() {
                            if flow.changed().await.is_err() {
                                break;
                            }
                        }
                        let data = crate::base64::encode(&buffer[..n]);
                        let sent = runner
                            .emit(&Event::Out {
                                shell,
                                stream,
                                seq: entry.next_seq(stream),
                                data,
                            })
                            .await;
                        if !sent {
                            // The client cannot hear this pipe any more.
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Dropped on every path out, including a pipe read error: `End` must
            // never wait for a signal that cannot come.
            let _ = drained.send(());
        });
    }

    /// Owns one shell's `Child` for as long as it exists, and is the only thing
    /// that ends it.
    ///
    /// # Invariant
    ///
    /// The leader is not reaped before its group is signalled: a reaped pid can be
    /// handed to another process, and the next signal would reach that one. See
    /// `docs/notes/server-remote.md`.
    fn supervise(
        self: &Arc<Self>,
        shell: u64,
        entry: Arc<Shell>,
        mut child: Child,
        mut closes: mpsc::Receiver<oneshot::Sender<CloseReport>>,
        drains: Vec<oneshot::Receiver<()>>,
    ) {
        let runner = self.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status.ok(),
                request = closes.recv() => {
                    entry.closing.store(true, Ordering::SeqCst);
                    // An in-flight write is drained before the signal: what it wrote
                    // was accepted while this shell was still open.
                    let _stdin = entry.stdin.lock().await;
                    let report = end_group(&entry).await;
                    let _ = child.start_kill();
                    let status = child.wait().await.ok();
                    if let Some(request) = request {
                        let _ = request.send(report);
                    }
                    status
                }
            };
            let code = status.and_then(|s| s.code());
            runner.shells.lock().await.remove(&shell);
            // Ends the flow control: a paused pipe still holds the shell's last
            // bytes, and those are what `End` waits for.
            entry.paused.send_replace(false);
            // Ends last: a pipe may still hold the shell's final bytes, and a client
            // told "ended" would stop reading them.
            for drained in drains {
                match tokio::time::timeout(PIPE_DRAIN, drained).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {}
                    Err(_) => tracing::warn!(
                        shell,
                        "a pipe was still open when the shell ended; its tail is lost"
                    ),
                }
            }
            runner.emit(&Event::End { shell, code }).await;
            // Anything asked after the shell ended is answered with what is true.
            while let Some(request) = closes.recv().await {
                let _ = request.send(CloseReport::unknown(format!(
                    "shell {shell} had already ended before the request arrived"
                )));
            }
        });
    }

    async fn write(&self, shell: u64, data: &str) -> Result<Value, String> {
        let entry = self.shell(shell).await?;
        let bytes = crate::base64::decode(data).ok_or("the write was not base64")?;
        let mut stdin = entry.stdin.lock().await;
        // Checked under the lock: a write this shell accepts is one `close` waits for.
        entry.open_for_work()?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|e| format!("could not write to shell {shell}: {e}"))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("could not flush shell {shell}: {e}"))?;
        Ok(json!({"written": bytes.len()}))
    }

    /// Stops or restarts one shell's pipes, which is how a slow client pushes back.
    async fn pause(&self, shell: u64, paused: bool) -> Result<Value, String> {
        let entry = self.shell(shell).await?;
        // A level every pump reads, so none can miss it.
        entry.paused.send_replace(paused);
        Ok(json!({"paused": paused}))
    }

    async fn signal(&self, shell: u64, signal: &str) -> Result<Value, String> {
        if !SIGNALS.contains(&signal) {
            return Err(format!("{signal} is not one of {}", SIGNALS.join(", ")));
        }
        let entry = self.shell(shell).await?;
        entry.open_for_work()?;
        let delivered = send(entry.pid, signal).await;
        Ok(json!({"delivered": delivered}))
    }

    /// Asks the supervisor to end the group, and answers only with its report.
    async fn close(&self, shell: u64) -> Result<Value, String> {
        let entry = self.shells.lock().await.get(&shell).cloned();
        // A shell that ended itself cannot be signalled: its pid may be reused.
        let Some(entry) = entry else {
            return Ok(CloseReport::unknown(format!("shell {shell} had already ended")).to_json(0));
        };
        let (reply, answer) = oneshot::channel();
        if entry.closer.send(reply).await.is_err() {
            return Ok(
                CloseReport::unknown(format!("shell {shell} has no supervisor")).to_json(entry.pid),
            );
        }
        let report = match tokio::time::timeout(CLOSE_ANSWER, answer).await {
            Ok(Ok(report)) => report,
            // Silence is not proof: a supervisor that did not answer proved nothing.
            Ok(Err(_)) => CloseReport::unknown(format!("shell {shell} stopped supervising")),
            Err(_) => CloseReport::unknown(format!("shell {shell} did not confirm in time")),
        };
        Ok(report.to_json(entry.pid))
    }

    async fn shell(&self, shell: u64) -> Result<Arc<Shell>, String> {
        self.shells
            .lock()
            .await
            .get(&shell)
            .cloned()
            .ok_or_else(|| format!("no shell {shell} is open"))
    }

    /// The client's channel is gone, so nothing here is anyone's to keep.
    ///
    /// # Invariant
    ///
    /// Every shell's group is ended and awaited before this returns; what could not
    /// be confirmed is logged, because the client that would be told is gone.
    async fn terminate_all(&self) {
        let shells: Vec<(u64, Arc<Shell>)> = self
            .shells
            .lock()
            .await
            .iter()
            .map(|(id, entry)| (*id, entry.clone()))
            .collect();
        for (id, entry) in shells {
            let (reply, answer) = oneshot::channel();
            if entry.closer.send(reply).await.is_err() {
                tracing::warn!(shell = id, pid = entry.pid, "no supervisor to end a shell");
                continue;
            }
            match tokio::time::timeout(CLOSE_ANSWER, answer).await {
                Ok(Ok(report)) if report.confirmed => {}
                Ok(Ok(report)) => tracing::warn!(
                    shell = id,
                    pid = entry.pid,
                    "a shell outlived this runner's channel, unconfirmed: {:?}",
                    report.unknown
                ),
                Ok(Err(_)) => tracing::warn!(shell = id, pid = entry.pid, "no close report"),
                Err(_) => tracing::warn!(
                    shell = id,
                    pid = entry.pid,
                    "a shell did not confirm its end before the runner exited"
                ),
            }
        }
    }
}

/// Ends one shell's whole process group and says whether that is proved.
///
/// # Invariant
///
/// Confirmed means the group was observed empty, not that a signal was delivered.
async fn end_group(entry: &Shell) -> CloseReport {
    let pid = entry.pid;
    if pid == 0 {
        return CloseReport::unknown("the shell never reported a pid");
    }
    #[cfg(unix)]
    {
        match group_alive(pid).await {
            Ok(false) => return CloseReport::confirmed(),
            Ok(true) => {}
            Err(e) => return CloseReport::unknown(format!("could not inspect the group: {e}")),
        }
        if send(pid, "TERM").await {
            match wait_group_gone(pid, TERM_GRACE).await {
                Ok(true) => return CloseReport::confirmed(),
                Ok(false) => {}
                Err(e) => return CloseReport::unknown(format!("could not confirm TERM: {e}")),
            }
        }
        if !send(pid, "KILL").await {
            return match group_alive(pid).await {
                Ok(false) => CloseReport::confirmed(),
                _ => CloseReport::unknown("the group took no KILL"),
            };
        }
        match wait_group_gone(pid, KILL_CONFIRM).await {
            Ok(true) => CloseReport::confirmed(),
            Ok(false) => CloseReport::unknown("the group was still there after KILL"),
            Err(e) => CloseReport::unknown(format!("could not confirm KILL: {e}")),
        }
    }
    #[cfg(not(unix))]
    {
        // No group to signal here: the job ends the tree and counts the proof.
        let Some(job) = entry.job.as_ref() else {
            return CloseReport::unknown("the shell was not in a job to end");
        };
        job.terminate();
        let deadline = std::time::Instant::now() + KILL_CONFIRM;
        while std::time::Instant::now() < deadline {
            match job.active_processes() {
                Some(0) => return CloseReport::confirmed(),
                Some(_) => tokio::time::sleep(SIGNAL_POLL).await,
                None => return CloseReport::unknown("the job could not be inspected"),
            }
        }
        match job.active_processes() {
            Some(0) => CloseReport::confirmed(),
            Some(left) => CloseReport::unknown(format!("{left} process(es) of the shell lived on")),
            None => CloseReport::unknown("the job could not be inspected"),
        }
    }
}

/// Whether any live member of `pid`'s group is left.
#[cfg(unix)]
async fn group_alive(pid: u32) -> anyhow::Result<bool> {
    let out = Command::new("ps")
        .args(["-e", "-o", "pgid=", "-o", "stat="])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("ps exited with {}", out.status);
    }
    // A group of zombies has no work left to stop.
    Ok(String::from_utf8_lossy(&out.stdout).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next().and_then(|p| p.parse::<u32>().ok()) == Some(pid)
            && fields.next().is_some_and(|stat| !stat.starts_with('Z'))
    }))
}

#[cfg(unix)]
async fn wait_group_gone(pid: u32, within: Duration) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if !group_alive(pid).await? {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(SIGNAL_POLL).await;
    }
}

/// One signal, to the whole group where the platform has one.
async fn send(pid: u32, signal: &str) -> bool {
    #[cfg(unix)]
    {
        crate::tools::detached::signal_group(pid, signal)
            .await
            .is_ok()
    }
    #[cfg(windows)]
    {
        // No signal numbers are reachable here: the verified tree kill is the
        // only verb, and TERM and KILL both mean it.
        let _ = signal;
        crate::tools::detached::kill_tree(pid).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, signal);
        false
    }
}

/// The peer's own disk, read by the peer.
///
/// # Invariant
///
/// Answered where the shells run: the machine that fills up is the only one that can
/// report how much it has left, and a server asking its own `statvfs` would answer
/// Starts this payload's relay for `dir`, detached, and answers its pid.
///
/// # Invariant
///
/// The relay must outlive this connection: it is started out of every job this
/// process is in, with its own stream to a log. It is what makes a channel on a peer
/// mean what a channel here means -- a program a restart does not end.
fn relay(dir: &str, cwd: Option<&str>) -> Result<Value, String> {
    let server = beside("server")?;
    let log = Path::new(dir).join("relay.log");
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(&server);
    command
        .arg("--relay")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .kill_on_drop(false);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    // Out of every job this connection is in: the runner is the only process that
    // can ask for that, and a channel that stayed in one would end with the ssh.
    #[cfg(windows)]
    let child = {
        const BREAKAWAY: u32 = 0x0100_0000;
        const DETACHED: u32 = 0x0000_0008;
        command.creation_flags(BREAKAWAY | DETACHED);
        match command.spawn() {
            Ok(child) => child,
            // A job that does not allow breakaway has nothing to break out of.
            Err(e) if e.raw_os_error() == Some(5) => {
                command.creation_flags(DETACHED);
                command
                    .spawn()
                    .map_err(|e| format!("starting the relay: {e}"))?
            }
            Err(e) => return Err(format!("starting the relay: {e}")),
        }
    };
    #[cfg(unix)]
    let child = {
        // SAFETY: `setsid` is async-signal-safe and touches nothing of this process.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        command
            .spawn()
            .map_err(|e| format!("starting the relay: {e}"))?
    };
    let pid = child.id().unwrap_or(0);
    // The child is this machine's now: this process must not wait for it, and must
    // not take it down on the way out.
    std::mem::drop(child);
    Ok(json!({"pid": pid, "log": log.display().to_string()}))
}

/// The program beside this one: the payload keeps them together.
fn beside(name: &str) -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| format!("this program's own path: {e}"))?;
    let dir = me
        .parent()
        .ok_or_else(|| "this program has no directory".to_string())?;
    // The payload names every program `gongga-<what>`, beside this one.
    let program = dir.join(format!("gongga-{name}{}", std::env::consts::EXE_SUFFIX));
    if program.is_file() {
        return Ok(program);
    }
    Err(format!(
        "no {name} beside {}: this payload does not carry a relay",
        me.display()
    ))
}

/// Appends bytes to a file, creating it and its parents.
///
/// # Invariant
///
/// Append, never rewrite: a channel's `in` is written by whoever speaks to it, and a
/// rewrite would drop the line another writer put there between two reads.
fn append(path: &str, data: &str) -> Result<Value, String> {
    let Some(bytes) = crate::base64::decode(data) else {
        return Err("append data is not base64".to_string());
    };
    let path = Path::new(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(json!({"bytes": bytes.len()}))
}

/// for the wrong machine. See `docs/notes/server-disk.md`.
fn space(path: &str) -> Result<Value, String> {
    let volumes = crate::disk::volumes(Path::new(path));
    let free = volumes.iter().filter_map(|volume| volume.free).min();
    Ok(json!({
        "free": free,
        "volumes": volumes
            .iter()
            .map(|volume| json!({ "label": volume.label, "free": volume.free }))
            .collect::<Vec<_>>(),
    }))
}

/// Whether `path` is there, and what it is; `exists: false` is an answer, not an error.
fn stat(path: &str) -> Result<Value, String> {
    let given = Path::new(path);
    match std::fs::metadata(given) {
        Ok(meta) => Ok(json!({
            "path": spelling(given),
            "exists": true,
            "is_file": meta.is_file(),
            "is_dir": meta.is_dir(),
            "size": meta.len(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({
            "path": path,
            "exists": false,
            "is_file": false,
            "is_dir": false,
            "size": 0,
        })),
        Err(e) => Err(format!("could not inspect {path:?} on the endpoint: {e}")),
    }
}

/// The endpoint's own spelling of a path: absolute, and with `/` throughout.
///
/// # Invariant
///
/// What this answers is what the server stores, hands back to `open`, and puts in a
/// shell word, so the three cannot disagree. Git Bash reports `/c/...` for a
/// directory it opened as `C:/...`, and a drive-less path is not one this machine
/// can open again; the verbatim prefix `canonicalize` adds is not one a shell reads.
fn spelling(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy().into_owned();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text).to_string();
    text.replace('\\', "/")
}

/// One chunk of a file, base64, with the offset the next read continues from.
async fn read(path: &str, offset: u64) -> Result<Value, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("could not open {path:?} on the endpoint: {e}"))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| format!("could not seek {path:?}: {e}"))?;
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]).await {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(format!("could not read {path:?} on the endpoint: {e}")),
        }
    }
    Ok(json!({
        "data": crate::base64::encode(&buffer[..filled]),
        "offset": offset + filled as u64,
        "eof": filled < buffer.len(),
    }))
}

/// One directory's entries, in name order so two calls read the same.
fn list(path: &str) -> Result<Value, String> {
    let entries = std::fs::read_dir(path)
        .map_err(|e| format!("could not list {path:?} on the endpoint: {e}"))?;
    let mut rows: Vec<Value> = entries
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().ok();
            json!({
                "name": name,
                "is_dir": file_type.as_ref().is_some_and(|t| t.is_dir()),
                "is_file": file_type.as_ref().is_some_and(|t| t.is_file()),
            })
        })
        .collect();
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!({"path": path, "entries": rows}))
}

/// Runs until the client's channel closes, then leaves nothing running.
///
/// # Invariant
///
/// A connection that can no longer be written to ends the run: shells are stopped
/// rather than left running where their output has nowhere to go.
pub async fn serve_stdio() -> anyhow::Result<()> {
    let runner = Arc::new(Runner {
        out: Mutex::new(tokio::io::stdout()),
        shells: Mutex::new(HashMap::new()),
        broken: CancellationToken::new(),
    });
    runner
        .emit(&Event::Hello {
            protocol: PROTOCOL,
            pid: std::process::id(),
        })
        .await;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = runner.broken.cancelled() => break,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "the runner's request channel failed");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                // The id cannot be known, so 0 is the only honest one.
                runner
                    .reply(0, Err(format!("malformed request: {e}")))
                    .await;
                continue;
            }
        };
        tokio::spawn(runner.clone().serve(request));
    }
    runner.terminate_all().await;
    Ok(())
}
