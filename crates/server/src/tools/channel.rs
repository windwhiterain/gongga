//! The channels: a program a session started, and what nobody has read of it since.
//!
//! See `docs/notes/server-channel.md`.
//!
//! # Invariant
//!
//! A channel is a process of this server's: its pipes end when the server ends, so a
//! channel does not outlive a restart the way a detached job does -- what survives a
//! restart is a job that writes to a file, not one whose output only this process can
//! read. A notice is therefore owed only while the server is up.
//!
//! Nothing here knows a program by name: `new` names a program and its words, and the
//! `util` table is what says how a channel of that name is started and framed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

/// What one call of `new` or `check` answers: [`SPEC`](SPEC.md), "Tool".
///
/// # Invariant
///
/// `output` is the window nobody has read yet, and taking it is what advances the
/// cursor: two calls never see the same bytes twice, and a notice carries the same
/// window as a `check` would have.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    pub output: String,
    /// Total time since the channel started, not since this call.
    pub time: Duration,
    /// Whether `until` was found. No `until` given is never a match.
    pub matched: bool,
    /// `None` while the program runs; the status once it ended.
    pub exit: Option<i32>,
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "output": self.output,
            "time": format!("{:.3}s", self.time.as_secs_f64()),
            "matched": self.matched,
            "exit": match self.exit {
                Some(code) => json!(code),
                None => Value::Null,
            },
        })
    }
}

/// What a channel says when it ends on its own, for the caller's session.
#[derive(Debug, Clone)]
pub struct ChannelEnd {
    pub session: String,
    pub id: String,
    pub output: String,
    pub time: Duration,
    pub exit: Option<i32>,
}

/// How a finished channel reaches its session: an inbox row, built by the caller.
pub type Notifier = Arc<dyn Fn(ChannelEnd) + Send + Sync>;

/// One open channel.
pub struct Channel {
    /// The path a caller names it by. `rename_job` is what moves it.
    id: std::sync::RwLock<String>,
    pub session: String,
    /// What the program was started as, for `ls_job` and for a message about it.
    pub program: String,
    /// Where it writes.
    stdin: Mutex<Option<ChildStdin>>,
    /// What nobody has read, and the cursor: taking it is reading it.
    unread: Mutex<String>,
    /// Woken whenever unread grew or the program ended.
    arrived: Notify,
    /// The program's status, once it ended.
    exit: Mutex<Option<i32>>,
    /// The process, so it can be killed. `None` once taken to be waited on.
    child: Mutex<Option<Child>>,
    started: Instant,
    ended: Mutex<bool>,
}

impl Channel {
    /// The path this channel is named by now.
    pub fn id(&self) -> String {
        self.id.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether this channel has ended, and what it ended with.
    pub async fn ended(&self) -> Option<i32> {
        *self.exit.lock().await
    }

    /// Whether anything is pending: unread bytes, or an end nobody has read.
    async fn ready(&self) -> bool {
        if self.ended().await.is_some() {
            return true;
        }
        !self.unread.lock().await.is_empty()
    }

    /// The next window: everything unread, or up to the end of `until` when it is
    /// there. Reading is what clears it.
    ///
    /// # Invariant
    ///
    /// The marker is not returned: `matched` is the answer to "was it there", and a
    /// caller that also had to read the marker out of the text would be parsing a
    /// convention twice.
    async fn take(&self, until: Option<&str>) -> (String, bool) {
        let mut unread = self.unread.lock().await;
        if let (Some(marker), Some(at)) = (until, until.and_then(|m| unread.find(m))) {
            let mut window = unread[..at].to_string();
            let rest = unread[at + marker.len()..].to_string();
            *unread = rest;
            trim_newline(&mut window);
            return (window, true);
        }
        (std::mem::take(&mut *unread), false)
    }

    /// Writes one line to the program: what an input is, until a `util` says
    /// otherwise. A channel whose program ended has no stdin left, and that is said.
    async fn write(&self, input: &str) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        let Some(handle) = stdin.as_mut() else {
            return Err(format!(
                "channel {:?} has no program to write to: it has ended",
                self.id()
            ));
        };
        handle
            .write_all(input.as_bytes())
            .await
            .map_err(|e| format!("could not write to channel {:?}: {e}", self.id()))?;
        handle
            .flush()
            .await
            .map_err(|e| format!("could not write to channel {:?}: {e}", self.id()))
    }

    /// Ends the program, and says whether it was still running.
    pub async fn stop(&self) -> bool {
        let mut child = self.child.lock().await;
        let Some(child) = child.as_mut() else {
            return false;
        };
        match child.start_kill() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(channel = %self.id(), "could not end the program: {e}");
                false
            }
        }
    }
}

/// A trailing newline the program's own echo left, not part of what it said.
fn trim_newline(window: &mut String) {
    if window.ends_with("\r\n") {
        window.truncate(window.len() - 2);
    } else if window.ends_with('\n') {
        window.truncate(window.len() - 1);
    }
}

/// What a call says when it will wait for nothing.
const NO_WAIT: Duration = Duration::ZERO;

/// How a program is started: what an input line becomes, and what ends a reply.
///
/// # Invariant
///
/// The one place a name is known. `bash` frames a reply with a sentinel and takes the
/// status out of it; `python` speaks a request and one reply; a name no row speaks of
/// is raw words, and a call that asked for `until` gets what arrives. See the `util`
/// table in `docs/notes/server-channel.md`.
#[derive(Debug, Clone, Default)]
pub struct Util {
    /// The words the program is started with, when a call gives no `args` of its own.
    pub spawn: Vec<String>,
    /// How an `input` is spelled: `{{input}}` stands where the input goes, and
    /// `{{sentinel}}` where this frame's marker goes.
    pub write: Option<String>,
    /// What ends a reply, when the call states no `until`.
    pub until: Option<String>,
    /// Whether the sentinel's own tail carries the status, which is then dropped
    /// from the window.
    pub exit_from_marker: bool,
}

/// Every channel this server holds, by id.
pub struct Channels {
    open: Mutex<HashMap<String, Arc<Channel>>>,
    /// What the config says each known name is.
    util: HashMap<String, Util>,
    /// Where a finished channel is announced.
    notify: Notifier,
    /// The chair this home's children sit in, so none is left behind.
    home: PathBuf,
    cancel: CancellationToken,
}

impl Channels {
    pub fn new(
        util: HashMap<String, Util>,
        notify: Notifier,
        home: PathBuf,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            open: Mutex::new(HashMap::new()),
            util,
            notify,
            home,
            cancel,
        }
    }

    /// The frames a name is spoken in.
    fn util_of(&self, name: &str) -> Util {
        self.util.get(name).cloned().unwrap_or_default()
    }

    /// Starts a program as a channel, or says why it could not.
    ///
    /// # Invariant
    ///
    /// The id is the path a caller names it by, and a path in use is refused: `new`
    /// creates, `check` speaks to what was created. See [`SPEC`](SPEC.md), "Tool".
    pub async fn start(
        &self,
        session: &str,
        name: &str,
        args: &[String],
        cwd: &std::path::Path,
        env: &[(String, String)],
        id: Option<String>,
    ) -> Result<Arc<Channel>, String> {
        let util = self.util_of(name);
        let id = id.unwrap_or_else(|| self.auto_id(session, name));
        {
            let open = self.open.lock().await;
            if open.contains_key(&id) {
                return Err(format!(
                    "channel {id:?} is in use; `check {id}` speaks to it, and `del_job {id}` ends it"
                ));
            }
        }
        let words: Vec<String> = if args.is_empty() {
            util.spawn.clone()
        } else {
            args.to_vec()
        };
        let mut command = Command::new(name);
        command
            .args(&words)
            .current_dir(cwd)
            .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start {name:?}: {e}"))?;
        let pid = child.id();
        // A child of this server ends with it: no channel is left writing into a
        // pipe whose reader is gone.
        if let Some(pid) = pid
            && !crate::tools::job::hold_served(pid)
        {
            tracing::warn!(channel = %id, pid, "a channel has no job object");
        }
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let channel = Arc::new(Channel {
            id: std::sync::RwLock::new(id.clone()),
            session: session.to_string(),
            program: name.to_string(),
            stdin: Mutex::new(stdin),
            unread: Mutex::new(String::new()),
            arrived: Notify::new(),
            exit: Mutex::new(None),
            child: Mutex::new(Some(child)),
            started: Instant::now(),
            ended: Mutex::new(false),
        });
        self.open
            .lock()
            .await
            .insert(id.clone(), Arc::clone(&channel));
        if let Some(stdout) = stdout {
            collect(Arc::clone(&channel), stdout);
        }
        if let Some(stderr) = stderr {
            collect(Arc::clone(&channel), stderr);
        }
        let watcher = Arc::clone(&channel);
        let notify = Arc::clone(&self.notify);
        tokio::spawn(async move {
            // The status is the last thing a program says: once it is in, the end is
            // news only if nobody asked for it.
            let mut child = watcher.child.lock().await.take();
            let Some(child) = child.as_mut() else {
                return;
            };
            let status = child.wait().await;
            let code = status.ok().and_then(|status| status.code());
            *watcher.exit.lock().await = Some(code.unwrap_or(0));
            *watcher.ended.lock().await = true;
            if let Some(pid) = pid {
                crate::tools::job::release(pid);
            }
            watcher.arrived.notify_waiters();
            notify(ChannelEnd {
                session: watcher.session.clone(),
                id: watcher.id(),
                output: watcher.take(None).await.0,
                time: watcher.started.elapsed(),
                exit: Some(code.unwrap_or(0)),
            });
        });
        Ok(channel)
    }

    /// The id a call that names none gets: the session's, then this program's, then a
    /// number that is free.
    fn auto_id(&self, session: &str, name: &str) -> String {
        format!("/{session}/{name}/{}", std::process::id())
    }

    /// Speaks to a channel: writes the input, then reads until the marker, the end
    /// of the program, or the end of the wait.
    ///
    /// # Invariant
    ///
    /// Waiting is the call's, never the `util`'s: `wait` absent is this server's
    /// ceiling and `wait: 0` is no wait at all. See [`SPEC`](SPEC.md), "Tool".
    pub async fn talk(
        &self,
        session: &str,
        id: &str,
        input: Option<&str>,
        wait: Duration,
        until: Option<&str>,
    ) -> Result<Outcome, String> {
        let channel = {
            let open = self.open.lock().await;
            match open.get(id) {
                Some(channel) => Arc::clone(channel),
                None => return Err(self.no_such(id)),
            }
        };
        if channel.session != session {
            return Err(self.no_such(id));
        }
        if let Some(input) = input {
            channel.write(input).await?;
        }
        let deadline = Instant::now() + wait;
        loop {
            if channel.ready().await || wait == NO_WAIT {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = tokio::time::timeout(left, channel.arrived.notified()).await;
        }
        let (output, matched) = channel.take(until).await;
        Ok(Outcome {
            id: id.to_string(),
            output,
            time: channel.started.elapsed(),
            matched,
            exit: channel.ended().await,
        })
    }

    /// What a call naming nothing is told: which channels this session has.
    fn no_such(&self, id: &str) -> String {
        format!("no channel {id:?} in this session; `ls_job` lists the ones it has")
    }

    /// The channels one session holds, as rows for `ls_job`.
    pub async fn list(&self, session: &str) -> Vec<Value> {
        let open = self.open.lock().await;
        let mut rows: Vec<Value> = Vec::new();
        for channel in open.values() {
            if channel.session != session {
                continue;
            }
            rows.push(json!({
                "id": channel.id(),
                "program": channel.program,
                "time": format!("{:.3}s", channel.started.elapsed().as_secs_f64()),
                "exit": match channel.ended().await {
                    Some(code) => json!(code),
                    None => Value::Null,
                },
                "unread": channel.unread.lock().await.len(),
            }));
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        rows
    }

    /// Ends a channel, and forgets it.
    pub async fn stop(&self, session: &str, id: &str) -> Result<bool, String> {
        let channel = {
            let open = self.open.lock().await;
            match open.get(id) {
                Some(channel) if channel.session == session => Arc::clone(channel),
                _ => return Err(self.no_such(id)),
            }
        };
        let stopped = channel.stop().await;
        self.open.lock().await.remove(id);
        Ok(stopped)
    }

    /// Gives a channel another path, which is how an auto id becomes a name a reader
    /// can remember.
    pub async fn rename(&self, session: &str, id: &str, new_id: &str) -> Result<(), String> {
        let mut open = self.open.lock().await;
        if !open
            .get(id)
            .is_some_and(|channel| channel.session == session)
        {
            return Err(self.no_such(id));
        }
        if open.contains_key(new_id) {
            return Err(format!("channel {new_id:?} is in use"));
        }
        let Some(channel) = open.remove(id) else {
            return Err(self.no_such(id));
        };
        *channel.id.write().unwrap_or_else(|e| e.into_inner()) = new_id.to_string();
        open.insert(new_id.to_string(), channel);
        Ok(())
    }

    /// Ends every channel of one session, for `del_session`.
    pub async fn stop_session(&self, session: &str) -> usize {
        let mut open = self.open.lock().await;
        let doomed: Vec<String> = open
            .values()
            .filter(|channel| channel.session == session)
            .map(|channel| channel.id())
            .collect();
        let mut stopped = 0;
        for id in doomed {
            if let Some(channel) = open.remove(&id)
                && channel.stop().await
            {
                stopped += 1;
            }
        }
        stopped
    }

    /// The chair every channel sits in: dropping it ends them with the server.
    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    /// Whether a `cancel` is ending this server, so a channel need not be announced.
    pub fn closing(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Reads one pipe into the channel's window until it ends.
///
/// # Invariant
///
/// Two pipes, one window: what a program says and what it complains about are one
/// stream to a reader, because a caller has one place to look.
fn collect<R>(channel: Arc<Channel>, mut pipe: R)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                    channel.unread.lock().await.push_str(&text);
                    channel.arrived.notify_waiters();
                }
                Err(e) => {
                    tracing::warn!(channel = %channel.id(), "reading a channel failed: {e}");
                    break;
                }
            }
        }
    });
}
