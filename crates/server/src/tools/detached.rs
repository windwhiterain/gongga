//! Commands the server started and is not waiting for.
//!
//! See `docs/notes/server-tools-detached.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;

use crate::process::parent_of;
use crate::store::{DetachedRow, Store};

/// TERM is a request, not proof of termination. KILL follows only after this grace.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(1);
/// Even KILL is not accepted as proof until the group and leader are observed gone.
const KILL_CONFIRM: Duration = Duration::from_secs(2);
/// How often the registry asks whether a job it holds has ended.
///
/// # Invariant
///
/// A job we spawned is asked exactly (its handle is the process); an adopted one is
/// asked by listing every pid once, so this is the rate of that listing, not of a
/// signal. See `docs/notes/server-tools-detached.md`.
const JOB_POLL: Duration = Duration::from_millis(500);
#[cfg(any(windows, unix))]
const SIGNAL_POLL: Duration = Duration::from_millis(25);
/// Unix `ps` reports elapsed whole seconds; leave room for that and spawn-to-insert latency.
const PROCESS_START_TOLERANCE_MS: u64 = 5_000;

/// What a caller needs to know about a job. Cheap to copy out of the registry.
#[derive(Debug, Clone)]
pub struct JobView {
    /// What the model calls this detached bash.
    pub name: String,
    pub pid: u32,
    /// The command it is running, so a name can be recognised without a lookup.
    pub cmd: String,
    pub log: String,
    pub started_at: i64,
    pub running: bool,
}

impl JobView {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "pid": self.pid,
            "cmd": self.cmd,
            "log": self.log,
            "started_at": self.started_at,
            "running": self.running,
        })
    }
}

/// One detached process.
pub struct DetachedJob {
    /// Row id, so stopping it can also forget it durably.
    pub id: i64,
    /// The name the model addresses this bash by, unique within the session.
    pub name: String,
    /// Row order in the log file name; never shown to the model.
    pub job: u64,
    /// The shell whose working directory it inherited.
    pub shell: String,
    pub pid: u32,
    pub cmd: String,
    /// Where its output is being written.
    pub log: PathBuf,
    pub started_at: i64,
    /// Present only while the server that started it runs; an adopted job has only a pid.
    child: Option<Child>,
    /// Whether the session has already been told this job ended.
    ///
    /// # Invariant
    ///
    /// Exactly one notice per job, so a session waiting on one is woken once and a
    /// job that is deleted after its end is not noticed twice.
    noticed: bool,
}

impl DetachedJob {
    /// Starts `cmd` on its own, writing its output to `log`, in a group of its own.
    pub fn launch(
        program: &Path,
        cwd: &Path,
        cmd: &str,
        log: &Path,
        env: &[(String, String)],
    ) -> anyhow::Result<Self> {
        let mut command = Command::new(program);
        command
            .arg("--norc")
            .arg("--noprofile")
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .env("TERM", "dumb");
        for (k, v) in env {
            command.env(k, v);
        }
        Self::spawn(command, log, cmd)
    }

    /// Starts one program with its words, on its own, writing to `log`.
    ///
    /// # Invariant
    ///
    /// No shell reads the words: a worker is named by the same spellings the server
    /// has, and a path with a space in it must reach it as one path.
    pub fn launch_program(
        program: &Path,
        args: &[String],
        cwd: &Path,
        log: &Path,
        env: &[(String, String)],
    ) -> anyhow::Result<Self> {
        let mut command = Command::new(program);
        command.args(args).current_dir(cwd);
        for (k, v) in env {
            command.env(k, v);
        }
        let label = format!("{} {}", program.display(), args.join(" "));
        Self::spawn(command, log, &label)
    }

    /// The one place a detached process is started: in a group of its own, with a log.
    fn spawn(mut command: Command, log: &Path, label: &str) -> anyhow::Result<Self> {
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = std::fs::File::create(log)?;
        let err = out.try_clone()?;
        command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .kill_on_drop(false);
        // Its own process group, so stopping the job can take the whole tree.
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        #[cfg(windows)]
        let mut child = {
            const NEW_GROUP: u32 = 0x0000_0200;
            const BREAKAWAY: u32 = 0x0100_0000;
            const SUSPENDED: u32 = 0x0000_0004;
            // Out of this server's job, and suspended: armed before the first fork.
            command.creation_flags(NEW_GROUP | BREAKAWAY | SUSPENDED);
            match command.spawn() {
                Ok(child) => child,
                // A job that does not allow breakaway has nothing to break out of.
                Err(e) if e.raw_os_error() == Some(5) => {
                    command.creation_flags(NEW_GROUP | SUSPENDED);
                    command
                        .spawn()
                        .map_err(|e| anyhow::anyhow!("spawning detached command: {e}"))?
                }
                Err(e) => return Err(anyhow::anyhow!("spawning detached command: {e}")),
            }
        };
        #[cfg(unix)]
        let child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning detached command: {e}"))?;
        // Armed before it runs: this tree ends with this server unless it is let go.
        #[cfg(windows)]
        {
            let pid = child.id().unwrap_or(0);
            if !crate::tools::job::arm(pid) {
                tracing::warn!(
                    pid,
                    "a detached process has no job object: it outlives this server whether \
                     or not anything asked it to"
                );
            }
            // A suspended process is resumed either way: a weaker promise, not a hang.
            if let Err(e) = crate::tools::job::ProcessJob::resume_main(pid) {
                let _ = child.start_kill();
                return Err(anyhow::anyhow!("resuming detached process {pid}: {e}"));
            }
        }
        let pid = child.id().unwrap_or(0);
        Ok(Self {
            id: 0,
            name: String::new(),
            job: 0,
            shell: String::new(),
            pid,
            cmd: label.to_string(),
            log: log.to_path_buf(),
            started_at: now_millis(),
            child: Some(child),
            noticed: false,
        })
    }

    /// Gives up the child to a caller who will wait on it itself.
    pub fn take_child(&mut self) -> Option<Child> {
        self.child.take()
    }

    /// Rebuilds a job from what a previous run left in the database.
    pub fn adopted(row: &DetachedRow) -> Self {
        Self {
            id: row.id,
            name: row.name.clone(),
            job: row.job,
            shell: row.shell.clone(),
            pid: row.pid as u32,
            cmd: row.cmd.clone(),
            log: PathBuf::from(&row.log),
            started_at: row.started_at,
            child: None,
            noticed: row.noticed,
        }
    }

    /// Stops it, taking the whole tree with it; true only once absence is confirmed.
    pub async fn stop(&mut self) -> bool {
        // The job object is the tree: it ends every member, which the walk approximates.
        disarm_detached(self.pid);
        if let Some(child) = &mut self.child {
            // An id is ours only while its handle lives: see
            // `docs/notes/server-tools-detached.md`.
            return match child.try_wait() {
                Ok(None) => kill_tree(self.pid).await && reap_child(child).await,
                Ok(Some(_)) => true,
                Err(e) => {
                    tracing::warn!(
                        pid = self.pid,
                        "could not ask whether a detached bash ended: {e}"
                    );
                    false
                }
            };
        }
        if is_self_or_ancestor(self.pid) {
            // A row from before `adopt` refused ancestors: killing the tree would kill us.
            tracing::warn!(
                pid = self.pid,
                "refusing to stop a detached job that is this server's parent"
            );
            return false;
        }
        match process_started_at(self.pid) {
            Ok(None) => {
                // The saved leader is gone, but its group can still hold children.
                kill_tree(self.pid).await && matches!(process_started_at(self.pid), Ok(None))
            }
            Ok(Some(actual)) if same_process(self.started_at, actual) => {
                kill_tree(self.pid).await && matches!(process_started_at(self.pid), Ok(None))
            }
            Ok(Some(actual)) => {
                tracing::warn!(
                    pid = self.pid,
                    saved_started_at = self.started_at,
                    actual_started_at = actual,
                    "refusing to stop an adopted pid whose identity changed"
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    pid = self.pid,
                    "could not verify adopted process identity: {e}"
                );
                false
            }
        }
    }

    /// This job as a caller sees it, from one listing that answered for every job.
    pub fn view(&mut self, live: &std::collections::HashSet<u32>) -> JobView {
        JobView {
            name: self.name.clone(),
            cmd: self.cmd.clone(),
            pid: self.pid,
            log: self.log.display().to_string(),
            started_at: self.started_at,
            running: match &mut self.child {
                // Exact: the handle is the process.
                Some(child) => matches!(child.try_wait(), Ok(None)),
                // Adopted across a restart: only the pid survived, and the
                // listing that answered for the whole set is already in hand.
                None => live.contains(&self.pid),
            },
        }
    }
}

/// How many jobs a stop request dealt with.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stopped {
    /// Processes confirmed gone.
    pub stopped: usize,
    /// Jobs we could neither find nor stop.
    pub failed: usize,
}

impl Stopped {
    fn one(ok: bool) -> Self {
        Self {
            stopped: usize::from(ok),
            failed: usize::from(!ok),
        }
    }

    pub fn total(&self) -> usize {
        self.stopped + self.failed
    }
}

impl std::ops::AddAssign for Stopped {
    fn add_assign(&mut self, other: Self) {
        self.stopped += other.stopped;
        self.failed += other.failed;
    }
}

/// The detached jobs of every session, shared by the tools and the commands.
#[derive(Clone)]
pub struct DetachedRegistry {
    inner: Arc<AsyncMutex<Registry>>,
}

struct Registry {
    /// session -> its jobs, oldest first.
    jobs: HashMap<String, Vec<DetachedJob>>,
    /// Names temporarily outside `jobs` while their processes stop off-lock.
    stopping: HashSet<(String, String)>,
    next_job: u64,
    program: Option<PathBuf>,
    logs: PathBuf,
    store: Store,
    /// Variables handed to every job, so a job that restarts the server can
    /// still say which one it is talking about.
    env: Vec<(String, String)>,
}

impl DetachedRegistry {
    pub fn new(
        store: Store,
        program: Option<PathBuf>,
        logs: PathBuf,
        env: Vec<(String, String)>,
    ) -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(Registry {
                jobs: HashMap::new(),
                stopping: HashSet::new(),
                next_job: 0,
                program,
                logs,
                store,
                env,
            })),
        }
    }

    /// Starts a detached bash called `name` for `session`, in `cwd`.
    ///
    /// # Invariant
    ///
    /// The name is unique within the session and is how the thing is later
    /// deleted: a detached bash nobody can name is one nobody can stop.
    /// See `docs/notes/server-tools-detached.md`.
    pub async fn start(
        &self,
        session: &str,
        name: &str,
        cmd: &str,
        cwd: &Path,
    ) -> anyhow::Result<JobView> {
        let mut registry = self.inner.lock().await;
        let Some(program) = registry.program.clone() else {
            anyhow::bail!("no bash available; set [tools.bash] program");
        };
        if registry
            .jobs
            .get(session)
            .is_some_and(|jobs| jobs.iter().any(|j| j.name == name))
            || registry
                .stopping
                .contains(&(session.to_string(), name.to_string()))
        {
            anyhow::bail!(
                "this session already has a detached bash named {name:?}; delete it with \
                 `del_bash name_regex: \"^{name}$\"` or pick another name"
            );
        }
        registry.next_job += 1;
        let job = registry.next_job;
        let log = registry.logs.join(format!("{session}-{job}.log"));
        // A job outlives the shell that started it, so its session is a fact
        // nothing inside it can learn any other way.
        let mut env = registry.env.clone();
        env.push(("GONGGA_SESSION".to_string(), session.to_string()));
        let mut launched = DetachedJob::launch(&program, cwd, cmd, &log, &env)?;
        let id = match registry.store.add_detached_job(&DetachedRow {
            id: 0,
            session: session.to_string(),
            name: name.to_string(),
            // It belongs to no shell, so its own name is the only true label.
            shell: name.to_string(),
            job,
            pid: launched.pid as i64,
            cmd: cmd.to_string(),
            log: log.display().to_string(),
            started_at: launched.started_at,
            noticed: false,
        }) {
            Ok(id) => id,
            Err(e) => {
                // The lock is dropped before terminating; if cleanup also fails, the
                // error says a process may remain.
                drop(registry);
                let reclaimed = launched.stop().await;
                if !reclaimed {
                    tracing::error!(
                        pid = launched.pid,
                        "could not reclaim a detached process whose database insert failed"
                    );
                }
                return Err(e.context(if reclaimed {
                    "recording detached job; the spawned process was stopped"
                } else {
                    "recording detached job; cleanup of the spawned process also failed"
                }));
            }
        };
        launched.id = id;
        launched.name = name.to_string();
        launched.job = job;
        launched.shell = name.to_string();
        // A just-launched job has its handle, so no listing is needed.
        let view = launched.view(&Default::default());
        registry
            .jobs
            .entry(session.to_string())
            .or_default()
            .push(launched);
        Ok(view)
    }

    /// Whether this session already answers to `name`, a job being stopped included.
    ///
    /// # Invariant
    ///
    /// Shells and detached bashes share the SPEC's key `{session, bash_name}`, so a
    /// caller must ask this before naming a shell.
    pub async fn holds(&self, session: &str, name: &str) -> bool {
        let registry = self.inner.lock().await;
        registry
            .jobs
            .get(session)
            .is_some_and(|jobs| jobs.iter().any(|j| j.name == name))
            || registry
                .stopping
                .contains(&(session.to_string(), name.to_string()))
    }

    /// Tells each session whose detached job ended, once, while this server runs.
    ///
    /// # Invariant
    ///
    /// The report is the row an `inject` notice is: a session that is busy reads it at
    /// its next step boundary and an idle one is woken for it, which is the only shape
    /// that reaches both. Nothing else notices a detached bash's end -- its output goes
    /// to a log nobody is watching. See `docs/notes/server-tools-detached.md`.
    pub fn watch(&self, stop: tokio_util::sync::CancellationToken) -> tokio::task::JoinHandle<()> {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(JOB_POLL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tick.tick() => registry.notice_ended().await,
                }
            }
        })
    }

    /// One pass over every job this registry holds.
    async fn notice_ended(&self) {
        let adopted: Vec<u32> = {
            let registry = self.inner.lock().await;
            registry
                .jobs
                .values()
                .flatten()
                .filter(|j| j.child.is_none() && !j.noticed)
                .map(|j| j.pid)
                .collect()
        };
        let live = if adopted.is_empty() {
            HashSet::new()
        } else {
            match live_batch(adopted.clone()).await {
                Ok(live) => live,
                Err(e) => {
                    // Unknown is not ended: a notice is a claim, and this cannot make it.
                    tracing::warn!("could not enumerate detached processes: {e}");
                    return;
                }
            }
        };
        let mut ended = Vec::new();
        {
            let mut registry = self.inner.lock().await;
            for (session, jobs) in registry.jobs.iter_mut() {
                for job in jobs.iter_mut() {
                    if job.noticed {
                        continue;
                    }
                    let code = match &mut job.child {
                        Some(child) => match child.try_wait() {
                            Ok(Some(status)) => Some(status.code()),
                            Ok(None) => continue,
                            Err(e) => {
                                tracing::warn!(pid = job.pid, error = %e,
                                    "could not ask whether a detached bash ended");
                                continue;
                            }
                        },
                        None => {
                            if live.contains(&job.pid) {
                                continue;
                            }
                            None
                        }
                    };
                    job.noticed = true;
                    release_ended(job.pid);
                    ended.push((
                        session.clone(),
                        job.id,
                        job.name.clone(),
                        job.pid,
                        job.log.clone(),
                        code,
                    ));
                }
            }
        }
        for (session, id, name, pid, log, code) in ended {
            let store = self.inner.lock().await.store.clone();
            notice_one_end(&store, &session, &name, pid, &log, code).await;
            // Written after the notice: a row that says it was noticed must have been.
            if let Err(e) = store.note_detached_noticed(id) {
                tracing::warn!(pid, error = %e, "could not record that a detached bash was noticed");
            }
        }
    }

    /// The jobs of one session, with their liveness.
    pub async fn list(&self, session: &str) -> Vec<JobView> {
        // Asked before taking the lock: one blocking listing for every adopted job.
        let pids: Vec<u32> = {
            let registry = self.inner.lock().await;
            match registry.jobs.get(session) {
                Some(jobs) => jobs
                    .iter()
                    .filter(|j| j.child.is_none())
                    .map(|j| j.pid)
                    .collect(),
                None => return Vec::new(),
            }
        };
        let live = match live_batch(pids.clone()).await {
            Ok(live) => live,
            Err(e) => {
                // `running` has no word for unknown, so assume alive: unlike a false
                // "dead", this can never forget a row.
                tracing::warn!("could not enumerate detached processes: {e}");
                pids.into_iter().collect()
            }
        };
        let mut registry = self.inner.lock().await;
        let Some(jobs) = registry.jobs.get_mut(session) else {
            return Vec::new();
        };
        jobs.iter_mut().map(|j| j.view(&live)).collect()
    }

    /// Stops every detached bash of `session` whose name matches `pattern`.
    ///
    /// # Invariant
    ///
    /// Only names are matched, never commands: deleting by command text would
    /// make a typo kill a process the caller never named.
    /// See `docs/notes/server-tools-detached.md`.
    pub async fn stop(&self, session: &str, pattern: &str) -> Stopped {
        let re = match regex::Regex::new(pattern) {
            Ok(re) => re,
            Err(_) => return Stopped::default(),
        };
        let (taken, names, store) = {
            let mut registry = self.inner.lock().await;
            let Some(jobs) = registry.jobs.get_mut(session) else {
                return Stopped::default();
            };
            let (taken, kept): (Vec<DetachedJob>, Vec<DetachedJob>) = std::mem::take(jobs)
                .into_iter()
                .partition(|j| re.is_match(&j.name));
            *jobs = kept;
            let names: Vec<String> = taken.iter().map(|j| j.name.clone()).collect();
            for name in &names {
                registry
                    .stopping
                    .insert((session.to_string(), name.clone()));
            }
            (taken, names, registry.store.clone())
        };
        // Waiting for a child while holding the registry lock stalls every session.
        let (out, retry) = stop_and_forget(&store, taken).await;
        self.restore(session, names, retry).await;
        out
    }

    /// Takes the jobs of a deleted session out of the registry, without ending them.
    ///
    /// # Invariant
    ///
    /// A delete waits for nothing, so what it cannot wait for it hands on: the caller
    /// spawns the stop, and the rows are already gone by then -- which leaves that task
    /// the only holder of what the session started, and nothing to retry for.
    /// See `docs/notes/server-tools-detached.md`.
    pub async fn detach_session(&self, session: &str) -> Vec<DetachedJob> {
        let mut registry = self.inner.lock().await;
        registry.jobs.remove(session).unwrap_or_default()
    }

    /// Ends every background job; a stop that is not a `cancel` owns this.
    /// Rows stay for the next start to report as gone.
    ///
    /// # Invariant
    ///
    /// Two passes, because two things can hold a tree: the armed job objects cover
    /// the workers this registry does not hold, and the pids here cover a process
    /// that failed to arm. Neither pass deletes a row -- a job killed while the
    /// server was down is still news for its session.
    pub async fn stop_all(&self) -> usize {
        #[cfg(windows)]
        let killed = crate::tools::job::terminate_all();
        #[cfg(not(windows))]
        let killed = 0;
        let jobs: Vec<DetachedJob> = {
            let mut registry = self.inner.lock().await;
            std::mem::take(&mut registry.jobs)
                .into_values()
                .flatten()
                .collect()
        };
        let known = jobs.len();
        for mut job in jobs {
            job.stop().await;
        }
        killed.max(known)
    }

    /// Puts failed stops back under their original names so the request can be retried.
    async fn restore(&self, session: &str, names: Vec<String>, retry: Vec<DetachedJob>) {
        let mut registry = self.inner.lock().await;
        for name in names {
            registry.stopping.remove(&(session.to_string(), name));
        }
        if !retry.is_empty() {
            let jobs = registry.jobs.entry(session.to_string()).or_default();
            jobs.extend(retry);
            jobs.sort_by_key(|j| j.job);
        }
    }

    /// Takes back the jobs a previous run left in the database.
    ///
    /// See `docs/notes/server-tools-detached.md`.
    pub async fn adopt(&self) -> usize {
        let rows = {
            let registry = self.inner.lock().await;
            match registry.store.detached_jobs() {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("could not read detached jobs: {e}");
                    return 0;
                }
            }
        };
        let pids: Vec<u32> = rows
            .iter()
            .filter_map(|r| u32::try_from(r.pid).ok())
            .collect();
        let live = match live_batch(pids).await {
            Ok(live) => live,
            Err(e) => {
                // Unknown is not dead: retain every row and adopt nothing until
                // a later start can enumerate and verify the processes.
                tracing::warn!("could not enumerate detached processes for adoption: {e}");
                return 0;
            }
        };
        let mut adopted = 0;
        let mut registry = self.inner.lock().await;
        // Job numbers must not be reused: a job adopted from the database has
        // to keep the number the previous run gave it.
        registry.next_job = registry
            .next_job
            .max(rows.iter().map(|r| r.job).max().unwrap_or(0));
        for row in rows {
            let Ok(pid) = u32::try_from(row.pid) else {
                tracing::warn!(id = row.id, pid = row.pid, "not adopting an invalid pid");
                continue;
            };
            // Dead rows are the cheap case: no walk up the parent chain for a process that is gone.
            if !live.contains(&pid) {
                // Say it: a session waiting here would wait for ever. Unless already said.
                if !row.noticed {
                    notice_gone(&registry.store, &row, "it is not running any more").await;
                }
                let _ = registry.store.delete_detached_job(row.id);
                continue;
            }
            if is_self_or_ancestor(pid) {
                tracing::warn!(
                    session = %row.session,
                    job = row.job,
                    pid,
                    "not adopting a detached job that is this server or one of its \
                     ancestors: `del_bash` kills the tree, and the tree contains us"
                );
                let _ = registry.store.delete_detached_job(row.id);
                continue;
            }
            match process_started_at(pid) {
                Ok(Some(actual)) if same_process(row.started_at, actual) => {}
                Ok(Some(actual)) => {
                    tracing::warn!(
                        session = %row.session,
                        pid,
                        saved_started_at = row.started_at,
                        actual_started_at = actual,
                        "not adopting a pid whose process identity does not match"
                    );
                    if !row.noticed {
                        notice_gone(
                            &registry.store,
                            &row,
                            "the pid now belongs to another process, so this one is gone",
                        )
                        .await;
                    }
                    let _ = registry.store.delete_detached_job(row.id);
                    continue;
                }
                Ok(None) => {
                    // It exited between the batch listing and identity lookup.
                    let _ = registry.store.delete_detached_job(row.id);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(pid, "could not verify a detached process for adoption: {e}");
                    continue;
                }
            }
            tracing::info!(
                session = %row.session,
                job = row.job,
                pid = row.pid,
                cmd = %row.cmd,
                "adopting a detached job left by the previous run"
            );
            arm_adopted(pid);
            registry
                .jobs
                .entry(row.session.clone())
                .or_default()
                .push(DetachedJob::adopted(&row));
            adopted += 1;
        }
        adopted
    }
}

/// Posts one session the news that a detached bash is over.
///
/// # Invariant
///
/// A deleted session owes nothing: its row would be stranded, and its name may be
/// reused by a different session later. See `docs/notes/server-tools-detached.md`.
async fn notice_one_end(
    store: &Store,
    session: &str,
    name: &str,
    pid: u32,
    log: &Path,
    code: Option<Option<i32>>,
) {
    if matches!(store.get_session(session), Ok(None)) {
        return;
    }
    let how = match code {
        Some(Some(code)) => format!("has finished with exit status {code}"),
        Some(None) => "has ended, though its exit status could not be read".to_string(),
        // Adopted across a restart: the pid is all that survived, and it is gone.
        None => "is no longer running, and how it ended cannot be known from here".to_string(),
    };
    let message = format!(
        "The detached bash `{name}` (pid {pid}) {how}. What it printed was never \
         collected -- read it at: {}. The row is still there: `list_bash` reports it \
         as not running, and `del_bash name_regex: \"^{name}$\"` removes it.",
        log.display()
    );
    if let Err(e) = store.add_pending_task(
        session,
        "inject",
        &json!({"session": session, "message": message, "notice": "detached"}),
    ) {
        tracing::warn!(session, name, error = %e, "could not notice a detached bash's end");
    }
}

/// Tells a session that the detached bash it was waiting for is gone.
async fn notice_gone(store: &Store, row: &DetachedRow, why: &str) {
    if matches!(store.get_session(&row.session), Ok(None)) {
        return;
    }
    let command = if row.cmd.trim().is_empty() {
        String::new()
    } else {
        format!(" It was running: {}", row.cmd.trim())
    };
    let message = format!(
        "The detached bash `{}` (pid {}) is gone: {why}.{command} What it printed \
         before that is at: {}. Nothing is waiting on it any more, so there is nothing \
         left to wait for.",
        row.name, row.pid, row.log
    );
    if let Err(e) = store.add_pending_task(
        &row.session,
        "inject",
        &json!({"session": row.session, "message": message, "notice": "detached"}),
    ) {
        tracing::warn!(session = %row.session, pid = row.pid, error = %e,
            "could not notice a detached bash that did not survive the restart");
    }
}

/// Deletes a row only after its process is confirmed gone.
///
/// # Invariant
///
/// Any failure keeps both copies so a later `del_bash`/`del_session` can retry the
/// same job.
async fn stop_and_forget(store: &Store, jobs: Vec<DetachedJob>) -> (Stopped, Vec<DetachedJob>) {
    let mut out = Stopped::default();
    let mut retry = Vec::new();
    for mut one in jobs {
        let stopped = one.stop().await;
        if !stopped {
            out += Stopped::one(false);
            retry.push(one);
            continue;
        }
        match store.delete_detached_job(one.id) {
            Ok(()) => out += Stopped::one(true),
            Err(e) => {
                tracing::warn!(id = one.id, "could not forget stopped detached job: {e}");
                out += Stopped::one(false);
                retry.push(one);
            }
        }
    }
    (out, retry)
}

/// Kills a process tree: the pid and everything it started.
///
/// See `docs/notes/server-tools-detached.md`.
pub(crate) async fn kill_tree(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .await;
        // MSYS descendants do not always die with `taskkill /T`: re-read the verified
        // tree and force each member left.
        let remaining = match windows_tree_pids(pid) {
            Ok(remaining) => remaining,
            Err(e) => {
                tracing::warn!(pid, "could not inspect detached process tree: {e}");
                return false;
            }
        };
        for member in remaining {
            let _ = Command::new("taskkill")
                .args(["/PID", &member.to_string(), "/T", "/F"])
                .output()
                .await;
        }
        return match wait_windows_tree_gone(pid, KILL_CONFIRM).await {
            Ok(gone) => gone,
            Err(e) => {
                tracing::warn!(pid, "could not confirm detached process tree exit: {e}");
                false
            }
        };
    }
    // Unix: TERM, grace, then KILL. Neither a delivered signal nor a dead leader
    // proves the group empty.
    #[cfg(unix)]
    {
        match process_group_alive(pid).await {
            Ok(false) => return true,
            Err(e) => {
                tracing::warn!(pid, "could not inspect detached process group: {e}");
                return false;
            }
            Ok(true) => {}
        }
        if let Err(e) = signal_group(pid, "TERM").await {
            tracing::warn!(pid, "could not TERM detached process group: {e}");
            return matches!(process_group_alive(pid).await, Ok(false));
        }
        match wait_group_gone(pid, TERM_GRACE).await {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(pid, "could not confirm TERM of detached process group: {e}");
                return false;
            }
        }
        if let Err(e) = signal_group(pid, "KILL").await {
            tracing::warn!(pid, "could not KILL detached process group: {e}");
            return matches!(process_group_alive(pid).await, Ok(false));
        }
        match wait_group_gone(pid, KILL_CONFIRM).await {
            Ok(gone) => gone,
            Err(e) => {
                tracing::warn!(pid, "could not confirm KILL of detached process group: {e}");
                false
            }
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        false
    }
}

/// Includes descendants whose leader has exited: ToolHelp retains their parent id.
#[cfg(windows)]
fn windows_tree_pids(pid: u32) -> anyhow::Result<Vec<u32>> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut processes = Vec::new();
        if Process32FirstW(snapshot, &mut entry) == 0 {
            let e = std::io::Error::last_os_error();
            CloseHandle(snapshot);
            return Err(e.into());
        }
        loop {
            processes.push((entry.th32ProcessID, entry.th32ParentProcessID));
            if Process32NextW(snapshot, &mut entry) == 0 {
                if GetLastError() != ERROR_NO_MORE_FILES {
                    let e = std::io::Error::last_os_error();
                    CloseHandle(snapshot);
                    return Err(e.into());
                }
                break;
            }
        }
        CloseHandle(snapshot);

        let mut tree = HashSet::from([pid]);
        loop {
            let before = tree.len();
            for (child, parent) in &processes {
                if *child != pid && tree.contains(parent) {
                    tree.insert(*child);
                }
            }
            if tree.len() == before {
                break;
            }
        }
        Ok(processes
            .iter()
            .filter(|(process, _)| tree.contains(process))
            .map(|(process, _)| *process)
            .collect())
    }
}

#[cfg(windows)]
async fn wait_windows_tree_gone(pid: u32, within: Duration) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if windows_tree_pids(pid)?.is_empty() {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(SIGNAL_POLL).await;
    }
}

/// Reaps without an unbounded wait; a leader that escaped its group is killed by
/// the handle after group cleanup.
async fn reap_child(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => return true,
        Ok(None) => {}
        Err(_) => return false,
    }
    let _ = child.start_kill();
    match tokio::time::timeout(KILL_CONFIRM, child.wait()).await {
        Ok(Ok(_)) => true,
        _ => matches!(child.try_wait(), Ok(Some(_))),
    }
}

#[cfg(unix)]
pub(crate) async fn signal_group(pid: u32, signal: &str) -> anyhow::Result<()> {
    let out = Command::new("kill")
        .args([format!("-{signal}"), format!("-{pid}")])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("kill -{signal} -{pid} exited with {}", out.status);
    }
    Ok(())
}

/// A group with only zombie entries has no work left to terminate.
#[cfg(unix)]
async fn process_group_alive(pid: u32) -> anyhow::Result<bool> {
    let out = Command::new("ps")
        .args(["-e", "-o", "pgid=", "-o", "stat="])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("ps exited with {}", out.status);
    }
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
        if !process_group_alive(pid).await? {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(SIGNAL_POLL).await;
    }
}

/// Whether `pid` is still the process that started at `started_at`.
///
/// # Invariant
///
/// The answer a caller needs before it touches a pid it did not start: a pid is only
/// unique until it is recycled.
pub fn names_process(pid: u32, started_at: i64) -> anyhow::Result<bool> {
    match process_started_at(pid) {
        Ok(Some(actual)) => Ok(same_process(started_at, actual)),
        Ok(None) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Puts a process a previous run left under this server's life.
///
/// # Invariant
///
/// Only once the pid is proved to be the process its row names: this arms a job that
/// kills on close, so a recycled pid would make this server's death somebody else's.
pub(crate) fn arm_adopted(pid: u32) {
    #[cfg(windows)]
    if !crate::tools::job::arm(pid) {
        tracing::warn!(
            pid,
            "could not put an adopted detached process under a job object: it ends with \
             this server only when the server lives to end it"
        );
    }
    #[cfg(not(windows))]
    let _ = pid;
}

/// Ends a process named only by its pid, and says whether it is gone.
///
/// # Invariant
///
/// The job object first (it ends every member exactly), then the walk, which is what
/// is left when nothing armed the tree. See `docs/notes/server-tools-detached.md`.
pub(crate) async fn stop_process(pid: u32) -> bool {
    let exact = disarm_detached(pid);
    kill_tree(pid).await || exact
}

/// Ends a detached tree now, when this server holds a job for it.
#[cfg(windows)]
pub fn disarm_detached(pid: u32) -> bool {
    crate::tools::job::disarm(pid)
}

/// No job object exists on this platform.
#[cfg(not(windows))]
pub fn disarm_detached(_pid: u32) -> bool {
    false
}

/// Lets every background job outlive this server: what `cancel` means for them.
///
/// # Invariant
///
/// `cancel` asked by a client is the only caller. Every other stop ends them
/// through `terminate_all`, so letting them go is an act of the ask rather than
/// a side effect of the process exiting.
#[cfg(windows)]
pub fn release_detached() -> usize {
    crate::tools::job::release_all()
}

/// No job object exists here, so there is nothing to let go of.
#[cfg(not(windows))]
pub fn release_detached() -> usize {
    0
}

/// Lets go of the tree a leader left behind, once its end is a fact.
///
/// # Invariant
///
/// Released, not killed: a leader that ended takes the job with it, so closing the job
/// there would end what it started in the background.
pub(crate) fn release_ended(pid: u32) {
    #[cfg(windows)]
    crate::tools::job::release(pid);
    #[cfg(not(windows))]
    let _ = pid;
}

fn same_process(saved_started_at: i64, actual_started_at: i64) -> bool {
    saved_started_at.abs_diff(actual_started_at) <= PROCESS_START_TOLERANCE_MS
}

/// OS process creation time: a recycled pid fails the check before any signal.
/// `Ok(None)` means absent, an error unknown.
#[cfg(windows)]
fn process_started_at(pid: u32) -> anyhow::Result<Option<i64>> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return Ok(None);
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            if GetLastError() == ERROR_INVALID_PARAMETER {
                return Ok(None);
            }
            return Err(std::io::Error::last_os_error().into());
        }
        let mut exit_code = 0;
        if GetExitCodeProcess(handle, &mut exit_code) == 0 {
            CloseHandle(handle);
            return Err(std::io::Error::last_os_error().into());
        }
        // A Windows process object remains openable while handles to it exist,
        // even after the process itself has exited.
        if exit_code != STILL_ACTIVE as u32 {
            CloseHandle(handle);
            return Ok(None);
        }
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        const WINDOWS_TO_UNIX_100NS: u64 = 116_444_736_000_000_000;
        if ticks < WINDOWS_TO_UNIX_100NS {
            anyhow::bail!("process creation time predates the Unix epoch");
        }
        Ok(Some(((ticks - WINDOWS_TO_UNIX_100NS) / 10_000) as i64))
    }
}

/// `ps etime` is available on Linux and macOS and avoids parsing locale dates.
#[cfg(unix)]
fn process_started_at(pid: u32) -> anyhow::Result<Option<i64>> {
    if pid == 0 {
        return Ok(None);
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "etime=", "-o", "stat=", "-p", &pid.to_string()])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("ps exited with {}", out.status);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mut fields = text.split_whitespace();
    let elapsed_text = fields.next().expect("nonempty ps output");
    let stat = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing process state in ps output {text:?}"))?;
    if stat.starts_with('Z') {
        return Ok(None);
    }
    let elapsed = parse_elapsed(elapsed_text)
        .ok_or_else(|| anyhow::anyhow!("invalid ps etime {elapsed_text:?}"))?;
    let millis = elapsed.saturating_mul(1_000).min(i64::MAX as u64) as i64;
    Ok(Some(now_millis().saturating_sub(millis)))
}

#[cfg(unix)]
fn parse_elapsed(text: &str) -> Option<u64> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let fields: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => (
            0,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        _ => return None,
    };
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    Some(days * 86_400 + hours * 3_600 + minutes * 60 + seconds)
}

#[cfg(not(any(windows, unix)))]
fn process_started_at(_pid: u32) -> anyhow::Result<Option<i64>> {
    anyhow::bail!("process creation time is unavailable on this platform")
}

/// Whether `pid` is this process or one of its ancestors.
///
/// See `docs/notes/server-tools-detached.md`.
fn is_self_or_ancestor(pid: u32) -> bool {
    let me = std::process::id();
    if pid == me {
        return true;
    }
    let mut child = me;
    // Bounded: a parent chain is a few links, and a recycled pid must not hang startup.
    for _ in 0..32 {
        match parent_of(child) {
            Some(parent) if parent != 0 && parent != child => {
                if parent == pid {
                    return true;
                }
                child = parent;
            }
            _ => break,
        }
    }
    false
}

/// `alive_pids` off the runtime's worker threads.
///
/// See `docs/notes/server-tools-detached.md`.
async fn live_batch(pids: Vec<u32>) -> anyhow::Result<HashSet<u32>> {
    if pids.is_empty() {
        return Ok(Default::default());
    }
    tokio::task::spawn_blocking(move || alive_pids(&pids))
        .await
        .map_err(|e| anyhow::anyhow!("joining process enumeration: {e}"))?
}

/// Which of `pids` are in use now, answered for all of them at once.
///
/// See `docs/notes/server-tools-detached.md`.
pub(crate) fn alive_pids(pids: &[u32]) -> anyhow::Result<HashSet<u32>> {
    let wanted: HashSet<u32> = pids.iter().copied().filter(|p| *p != 0).collect();
    if wanted.is_empty() {
        return Ok(wanted);
    }
    #[cfg(windows)]
    {
        // No `/FI`: one call for the whole set. CSV puts the pid in the second field.
        let out = std::process::Command::new("tasklist")
            .args(["/NH", "/FO", "CSV"])
            .output()?;
        if !out.status.success() {
            anyhow::bail!("tasklist exited with {}", out.status);
        }
        return Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split(',').nth(1))
            .filter_map(|f| f.trim().trim_matches('"').parse::<u32>().ok())
            .filter(|pid| wanted.contains(pid))
            .collect());
    }
    #[cfg(unix)]
    {
        // `ps` prints every pid at once; the trailing `=` drops the header.
        let out = std::process::Command::new("ps")
            .args(["-e", "-o", "pid="])
            .output()?;
        if !out.status.success() {
            anyhow::bail!("ps exited with {}", out.status);
        }
        return Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .filter(|pid| wanted.contains(pid))
            .collect());
    }
    #[cfg(not(any(windows, unix)))]
    {
        anyhow::bail!("process enumeration is unavailable on this platform")
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
