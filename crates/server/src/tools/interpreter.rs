//! One named interpreter, and the handles that survive its lock.
//!
//! See `docs/notes/server-tools-python.md`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use crate::tools::bash::BackgroundNotifier;
use crate::tools::python::{Repl, Reply};

/// One interpreter a session may name.
///
/// # Invariant
///
/// The counters and the stop handle are outside `inner`: a background call holds that
/// lock for as long as its code runs, and what tells a caller the name is busy, or
/// stops it, must not wait for the work it is asking about.
pub struct Interpreter {
    pub inner: Arc<AsyncMutex<Repl>>,
    /// Calls running in the background under this name.
    pub jobs: Arc<AtomicUsize>,
    /// What finished calls printed and nobody has read: `check_python` takes it.
    pub pending: Arc<StdMutex<String>>,
    next_job: AtomicU64,
    control: Control,
    facts: Facts,
}

/// What an interpreter was born with, readable while a call holds its lock.
///
/// # Invariant
///
/// Fixed at spawn: asking what name, machine or directory this is must not wait for
/// the call running in it -- that wait is what let a busy name look idle.
pub struct Facts {
    pub machine: crate::session::Machine,
    pub pid: Option<u32>,
    pub program: String,
    pub cwd: std::path::PathBuf,
}

/// How an interpreter is stopped without taking its lock.
#[derive(Clone)]
pub enum Control {
    Local(Arc<AsyncMutex<tokio::process::Child>>),
    Remote(crate::remote::transport::ShellControl),
}

impl Control {
    /// Ends the interpreter; the next call under that name starts a fresh one.
    pub async fn kill(&self) {
        match self {
            Control::Local(child) => {
                let mut child = child.lock().await;
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            Control::Remote(shell) => {
                let _ = shell.close().await;
            }
        }
    }
}

impl Interpreter {
    pub fn new(inner: Repl, control: Control, facts: Facts) -> Arc<Self> {
        Arc::new(Self {
            facts,
            inner: Arc::new(AsyncMutex::new(inner)),
            jobs: Arc::new(AtomicUsize::new(0)),
            pending: Arc::new(StdMutex::new(String::new())),
            next_job: AtomicU64::new(0),
            control,
        })
    }

    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    pub fn jobs(&self) -> usize {
        self.jobs.load(Ordering::SeqCst)
    }

    /// What finished calls printed and nobody has read; reading clears it.
    pub fn take_pending(&self) -> String {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut pending)
    }

    /// Ends the interpreter, and with it whatever it was running.
    pub async fn kill(&self) {
        self.control.kill().await;
    }

    /// Runs one call with no clock, reporting into this name's unread buffer.
    ///
    /// # Invariant
    ///
    /// The answer is an event, not a timeout: a background call is stopped by the
    /// interpreter ending or by `del_python`, never by this side giving up on it.
    pub fn spawn_background(self: &Arc<Self>, request: Value, notify: Option<BackgroundNotifier>) {
        let job = self.next_job.fetch_add(1, Ordering::SeqCst) + 1;
        let this = self.clone();
        self.jobs.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let outcome = {
                let mut repl = this.inner.lock().await;
                repl.run_untimed(&request).await
            };
            let (text, code) = match outcome {
                Ok(reply) => (reply_text(&reply), reply.code()),
                Err(e) => (format!("{e}"), None),
            };
            crate::tools::bash::collect(&this.pending, &text);
            // Counted as ended before anyone is told, so no notice outruns the buffer.
            this.jobs.fetch_sub(1, Ordering::SeqCst);
            if let Some(notify) = notify {
                notify(crate::tools::bash::BackgroundEnd {
                    job,
                    code,
                    ran: started.elapsed(),
                });
            }
        });
    }

    /// Waits until nothing runs here, or until `wait` passes.
    ///
    /// One clock, no token: `Tools::run` ends a wait.
    pub async fn wait_idle(&self, wait: Duration) {
        let deadline = std::time::Instant::now() + wait;
        while self.jobs() > 0 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return;
            }
            tokio::time::sleep(left.min(Duration::from_millis(100))).await;
        }
    }
}

/// The text one reply produced, stdout and stderr in the order a caller reads them.
pub fn reply_text(reply: &Reply) -> String {
    let mut text = reply.stdout.clone();
    if !reply.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&reply.stderr);
    }
    if !reply.ok && !text.contains("Traceback") {
        text.push_str("\n(the block raised)");
    }
    if text.is_empty() {
        "(no output)".to_string()
    } else {
        text
    }
}
