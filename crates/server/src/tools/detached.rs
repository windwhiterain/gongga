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

use crate::store::{DetachedRow, Store};

/// TERM is a request, not proof of termination. KILL follows only after this grace.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(1);
/// Even KILL is not accepted as proof until the group and leader are observed gone.
const KILL_CONFIRM: Duration = Duration::from_secs(2);
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
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = std::fs::File::create(log)?;
        let err = out.try_clone()?;

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
        {
            command.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
        }
        let child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning detached command: {e}"))?;
        let pid = child.id().unwrap_or(0);
        Ok(Self {
            id: 0,
            name: String::new(),
            job: 0,
            shell: String::new(),
            pid,
            cmd: cmd.to_string(),
            log: log.to_path_buf(),
            started_at: now_millis(),
            child: Some(child),
        })
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
        }
    }

    /// Stops it, taking the whole tree with it; true only once absence is confirmed.
    pub async fn stop(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            // A dead leader proves nothing about its group: terminate the group
            // first, then reap or kill the leader.
            let tree_stopped = match child.id() {
                Some(pid) => {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        // The handle covers reuse while the leader lives; see
                        // `docs/notes/server-tools-detached.md`.
                        match process_started_at(pid) {
                            Ok(None) => {}
                            Ok(Some(actual)) => {
                                tracing::warn!(
                                    pid,
                                    actual_started_at = actual,
                                    "refusing to signal a reused detached process group"
                                );
                                return false;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    pid,
                                    "could not verify an exited leader's group: {e}"
                                );
                                return false;
                            }
                        }
                    }
                    kill_tree(pid).await
                }
                None => false,
            };
            let leader_stopped = reap_child(child).await;
            return tree_stopped && leader_stopped;
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

    /// Stops every job of a session that is being deleted; `cancel` leaves them alone.
    pub async fn stop_session(&self, session: &str) -> Stopped {
        let (jobs, names, store) = {
            let mut registry = self.inner.lock().await;
            let jobs = registry.jobs.remove(session).unwrap_or_default();
            let names: Vec<String> = jobs.iter().map(|j| j.name.clone()).collect();
            for name in &names {
                registry
                    .stopping
                    .insert((session.to_string(), name.clone()));
            }
            (jobs, names, registry.store.clone())
        };
        let (out, retry) = stop_and_forget(&store, jobs).await;
        self.restore(session, names, retry).await;
        out
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

    /// Jobs still known, across every session.
    pub async fn count(&self) -> usize {
        self.inner.lock().await.jobs.values().map(Vec::len).sum()
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
async fn signal_group(pid: u32, signal: &str) -> anyhow::Result<()> {
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

#[cfg(test)]
fn pid_alive(pid: u32) -> bool {
    alive_pids(&[pid]).is_ok_and(|live| live.contains(&pid))
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

/// The process that started `pid`, as the OS sees it.
#[cfg(windows)]
fn parent_of(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // One snapshot per hop, not per walk: this runs on the startup path.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            if entry.th32ProcessID == pid {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        found
    }
}

/// The process that started `pid`, as the OS sees it.
#[cfg(unix)]
fn parent_of(pid: u32) -> Option<u32> {
    // One field, no header, and present wherever the shell is; `/proc` would be
    // faster but is not shaped the same on macOS.
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(not(any(windows, unix)))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
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
fn alive_pids(pids: &[u32]) -> anyhow::Result<HashSet<u32>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::bash::{BashShell, msys_path};
    use crate::tools::testbed::wait_until;
    use std::time::Duration;

    /// Long enough for a shell to start on a loaded machine, short enough that
    /// a hang is reported instead of inherited.
    const WAIT: Duration = Duration::from_secs(10);

    fn registry(dir: &Path, program: Option<PathBuf>) -> DetachedRegistry {
        let store = Store::open(&dir.join("g.db")).expect("store");
        DetachedRegistry::new(store, program, dir.join("logs"), vec![])
    }

    /// One call answers the whole set: our own pid in it, a bogus pid out of it.
    #[test]
    fn liveness_is_answered_for_many_pids_at_once() {
        let me = std::process::id();
        let absent = 4_000_000u32; // above any pid a run of this suite reaches
        let live = alive_pids(&[me, absent, 0]).expect("process listing");
        assert!(live.contains(&me), "own pid must be reported alive");
        assert!(!live.contains(&absent), "a bogus pid must not be");
        assert!(!live.contains(&0), "pid 0 is not a process");
        assert!(alive_pids(&[]).expect("empty listing").is_empty());
    }

    /// The defining property: a detached job is not the shell's, so losing the
    /// shell does not stop it.
    #[tokio::test]
    async fn a_detached_job_outlives_the_shell_it_came_from() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let flag = dir.path().join("finished.flag");
        let script = format!("sleep 0.2; echo done > '{}'", msys_path(&flag));

        let shell = match BashShell::spawn(&program, dir.path(), &[]) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        let cwd = shell.cwd().to_path_buf();
        let reg = registry(dir.path(), Some(program.clone()));
        let view = reg
            .start("main", "default", &script, &cwd)
            .await
            .expect("start");
        assert!(view.running, "it is alive the moment it is started");

        // The shell it was started from is gone; the job does not notice.
        drop(shell);
        let listed = reg.list("main").await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].running);

        wait_until("the job to write its flag", WAIT, || flag.exists()).await;
        wait_until("the job's process to end", WAIT, || !pid_alive(view.pid)).await;
        // Finished, but still listed: the log is why the row is kept.
        let listed = reg.list("main").await;
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].running, "it has finished by now");
    }

    /// A stopped job is gone and forgotten: a restart would not adopt it back.
    #[tokio::test]
    async fn stopping_a_job_takes_the_tree_with_it() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let flag = dir.path().join("finished.flag");
        let script = format!("sleep 5; echo done > '{}'", msys_path(&flag));
        let reg = registry(dir.path(), Some(program));

        let view = reg
            .start("main", "build", &script, dir.path())
            .await
            .expect("start");
        let stopped = reg.stop("main", "^build$").await;
        assert_eq!(stopped.stopped, 1, "the job should have been stopped");
        assert!(
            reg.list("main").await.is_empty(),
            "a stopped job is forgotten"
        );
        assert!(
            !pid_alive(view.pid),
            "the process it named is still running"
        );
        // A missed stop shows up as work that finishes: read the absence later.
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!flag.exists(), "the detached job was not stopped");
    }

    /// Neither `del_bash` nor `del_session` may forget running or unknown work; our
    /// own pid makes the stop fail safely.
    #[tokio::test]
    async fn failed_stops_remain_retryable_for_del_bash_and_del_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("g.db")).expect("store");
        let mut row = DetachedRow {
            id: 0,
            session: "main".into(),
            name: "unsafe".into(),
            shell: "unsafe".into(),
            job: 1,
            pid: std::process::id() as i64,
            cmd: "not really a job".into(),
            log: "x.log".into(),
            started_at: now_millis(),
        };
        row.id = store.add_detached_job(&row).expect("record job");
        let reg = DetachedRegistry::new(store.clone(), None, dir.path().join("logs"), vec![]);
        reg.inner
            .lock()
            .await
            .jobs
            .entry("main".into())
            .or_default()
            .push(DetachedJob::adopted(&row));

        let stopped = reg.stop("main", "^unsafe$").await;
        assert_eq!(stopped.failed, 1);
        assert_eq!(stopped.stopped, 0);
        assert_eq!(reg.count().await, 1, "a failed name match was restored");
        assert_eq!(store.detached_jobs().expect("rows").len(), 1);

        let stopped = reg.stop_session("main").await;
        assert_eq!(stopped.failed, 1);
        assert_eq!(reg.count().await, 1, "del_session retains a failed stop");
        assert_eq!(store.detached_jobs().expect("rows").len(), 1);
    }

    /// The whole point of recording pids: work that outlives the server can
    /// still be found, listed and stopped afterwards.
    #[tokio::test]
    async fn jobs_from_a_previous_run_are_adopted() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("g.db")).expect("store");

        // Stdio closed on purpose: a child holding our stdout keeps `cargo test` waiting.
        let mut held = std::process::Command::new(&program)
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawning something long-lived");
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "held".into(),
                shell: "held".into(),
                job: 7,
                pid: held.id() as i64,
                cmd: "sleep 5".into(),
                log: dir.path().join("logs/7.log").display().to_string(),
                started_at: now_millis(),
            })
            .expect("recording a live job");
        // A pid nobody owns: finished work, not to be adopted back.
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "gone".into(),
                shell: "gone".into(),
                job: 8,
                pid: 999_983,
                cmd: "gone".into(),
                log: dir.path().join("logs/8.log").display().to_string(),
                started_at: 0,
            })
            .expect("recording a finished job");

        let fresh = DetachedRegistry::new(store.clone(), None, dir.path().join("logs"), vec![]);
        assert_eq!(fresh.adopt().await, 1, "only the live job is adopted");
        let listed = fresh.list("main").await;
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].name, "held",
            "it keeps the name the previous run gave it"
        );
        assert!(listed[0].running);

        // An adopted job has no handle, only a pid, and can still be stopped.
        let stopped = fresh.stop("main", "^held$").await;
        assert_eq!(stopped.stopped, 1, "an adopted job is still stoppable");
        wait_until("the adopted process to end", WAIT, || !pid_alive(held.id())).await;
        let _ = held.kill();
        let _ = held.wait();
    }

    /// A uniqueness failure after spawn must not leave an unrecorded process.
    #[tokio::test]
    async fn a_failed_database_insert_reclaims_the_spawned_process() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("g.db");
        let store = Store::open(&db).expect("store");
        rusqlite::Connection::open(&db)
            .expect("schema connection")
            .execute_batch(
                "CREATE UNIQUE INDEX test_detached_name ON detached_jobs(session, name);",
            )
            .expect("unique test index");
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "blocked".into(),
                shell: "blocked".into(),
                job: 99,
                pid: 0,
                cmd: "existing row".into(),
                log: "x.log".into(),
                started_at: now_millis(),
            })
            .expect("blocking row");
        let reg = DetachedRegistry::new(
            store.clone(),
            Some(program),
            dir.path().join("logs"),
            vec![],
        );
        let pid_file = dir.path().join("spawned.pid");
        let script = format!("echo $$ > '{}'; sleep 30", msys_path(&pid_file));

        let error = reg
            .start("main", "blocked", &script, dir.path())
            .await
            .expect_err("the duplicate database name must fail");
        assert!(
            error.to_string().contains("was stopped"),
            "cleanup was not confirmed: {error}"
        );
        // Cleanup can win before the shell's first command runs; if it wrote a pid,
        // verify that process is gone too.
        if pid_file.exists() {
            let pid: u32 = std::fs::read_to_string(pid_file)
                .expect("pid file")
                .trim()
                .parse()
                .expect("numeric pid");
            wait_until("the unrecorded process to be reclaimed", WAIT, || {
                !pid_alive(pid)
            })
            .await;
        }
        assert_eq!(reg.count().await, 0);
        assert_eq!(store.detached_jobs().expect("rows").len(), 1);
    }

    /// A live pid with a different creation time is not our job and is never signaled.
    #[tokio::test]
    async fn adoption_rejects_a_process_identity_mismatch() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("g.db")).expect("store");
        let mut held = std::process::Command::new(&program)
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawning something long-lived");
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "recycled".into(),
                shell: "recycled".into(),
                job: 9,
                pid: held.id() as i64,
                cmd: "sleep 30".into(),
                log: "x.log".into(),
                started_at: now_millis() + 60_000,
            })
            .expect("record mismatched job");
        let fresh = DetachedRegistry::new(store.clone(), None, dir.path().join("logs"), vec![]);

        assert_eq!(fresh.adopt().await, 0);
        assert!(fresh.list("main").await.is_empty());
        assert_eq!(store.detached_jobs().expect("rows").len(), 1);
        assert!(pid_alive(held.id()), "the mismatched process was killed");
        let _ = held.kill();
        let _ = held.wait();
    }

    /// Our own pid must never be adopted: `del_bash` would then be able to
    /// shut the server down from inside.
    #[tokio::test]
    async fn a_server_never_adopts_its_own_pid() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("g.db")).expect("store");
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "svc1".into(),
                shell: "svc1".into(),
                job: 1,
                pid: std::process::id() as i64,
                cmd: "gongga-server --bind 127.0.0.1:9800".into(),
                log: "x.log".into(),
                started_at: 0,
            })
            .expect("recording our own launch");
        let fresh = DetachedRegistry::new(store.clone(), None, dir.path().join("logs"), vec![]);
        assert_eq!(fresh.adopt().await, 0, "we must not adopt ourselves");
        assert!(store.detached_jobs().expect("rows").is_empty());
    }

    /// Our pid is the `exec` case, the parent the spawn-and-wait case.
    ///
    /// See `docs/notes/server-tools-detached.md`.
    #[cfg(any(windows, unix))]
    #[tokio::test]
    async fn a_server_never_adopts_a_job_that_is_its_own_parent() {
        let parent = parent_of(std::process::id()).expect("a test process has a parent");
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("g.db")).expect("store");
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "svc3".into(),
                shell: "svc3".into(),
                job: 3,
                pid: parent as i64,
                cmd: "bash ./gongga-server &".into(),
                log: "x.log".into(),
                started_at: 0,
            })
            .expect("recording our parent");

        let fresh = DetachedRegistry::new(store.clone(), None, dir.path().join("logs"), vec![]);
        assert_eq!(
            fresh.adopt().await,
            0,
            "a parent is not a job we may manage"
        );
        assert!(
            store.detached_jobs().expect("rows").is_empty(),
            "the row must go, so nothing lists a job that would kill us"
        );
        assert!(fresh.list("main").await.is_empty());
    }

    /// TERM can be ignored: the registry stays free during the grace, then KILL is
    /// confirmed before the row goes.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_stop_escalates_term_without_holding_the_registry_lock() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let ready = dir.path().join("ready");
        let script = format!(
            "trap '' TERM; echo ready > '{}'; while :; do sleep 0.1; done",
            ready.display()
        );
        let reg = registry(dir.path(), Some(program));
        let view = reg
            .start("main", "stubborn", &script, dir.path())
            .await
            .expect("start");
        wait_until("the TERM trap to be installed", WAIT, || ready.exists()).await;

        let stopping = tokio::spawn({
            let reg = reg.clone();
            async move { reg.stop("main", "^stubborn$").await }
        });
        tokio::task::yield_now().await;
        tokio::time::timeout(Duration::from_millis(250), reg.count())
            .await
            .expect("stop held the global registry lock");
        let stopped = stopping.await.expect("stop task");
        assert_eq!(stopped.stopped, 1);
        assert!(!pid_alive(view.pid));
    }

    /// Reaping the shell leader must not skip descendants still in its group.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_stop_checks_the_group_after_the_leader_exits() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let pid_file = dir.path().join("child.pid");
        let script = format!("sleep 30 & echo $! > '{}'", msys_path(&pid_file));
        let mut job = DetachedJob::launch(
            &program,
            dir.path(),
            &script,
            &dir.path().join("x.log"),
            &[],
        )
        .expect("launch");
        wait_until("the descendant pid to be written", WAIT, || {
            pid_file.exists()
        })
        .await;
        let descendant: u32 = std::fs::read_to_string(pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("numeric pid");
        wait_until("the leader to exit", WAIT, || {
            matches!(job.child.as_mut().expect("child").try_wait(), Ok(Some(_)))
        })
        .await;
        assert!(pid_alive(descendant));

        assert!(job.stop().await, "the surviving group must be stopped");
        wait_until("the orphaned descendant to die", WAIT, || {
            !pid_alive(descendant)
        })
        .await;
    }

    #[cfg(unix)]
    #[test]
    fn parses_portable_ps_elapsed_times() {
        assert_eq!(parse_elapsed("01:02"), Some(62));
        assert_eq!(parse_elapsed("03:04:05"), Some(11_045));
        assert_eq!(parse_elapsed("2-03:04:05"), Some(183_845));
        assert_eq!(parse_elapsed("not-a-time"), None);
    }
}
