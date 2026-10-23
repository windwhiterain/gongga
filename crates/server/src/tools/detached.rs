//! Commands the server started and is not waiting for.
//!
//! `detached` is the strongest of the three ways `bash` can run, and it is
//! defined by what does *not* happen to it:
//!
//! * the call returns at once, so the turn is never blocked;
//! * nothing is collected -- output goes to a log file, not to the model;
//! * it outlives the shell it was started from, the session, and the server
//!   process itself.
//!
//! That last point is the reason this module exists. A detached job is how an
//! agent restarts the server it is running on: launch the replacement
//! detached, then `cancel`. Anything the server owned would die with it, so a
//! detached job is deliberately *not* owned -- it is a process the server
//! knows about, keeps in the database, and can stop later, including after the
//! server that started it is gone.
//!
//! Being killable across restarts is why the pid is recorded rather than the
//! handle alone: a handle dies with the process holding it, a pid can be
//! re-adopted. `adopt` is what takes the rows left by a previous run and makes
//! them manageable again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::json;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;

use crate::store::{DetachedRow, Store};

/// What a caller needs to know about a job. Cheap to copy out of the registry.
#[derive(Debug, Clone)]
pub struct JobView {
    pub job: u64,
    pub shell: String,
    pub pid: u32,
    pub cmd: String,
    pub log: String,
    pub started_at: i64,
    pub running: bool,
}

impl JobView {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "job": self.job,
            "shell": self.shell,
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
    /// Job number, unique within the session and shown to the model.
    pub job: u64,
    /// The shell whose working directory it inherited.
    pub shell: String,
    pub pid: u32,
    pub cmd: String,
    /// Where its output is being written.
    pub log: PathBuf,
    pub started_at: i64,
    /// Present only while the server that started it is still running; a job
    /// adopted from the database has a pid and nothing else.
    child: Option<Child>,
}

impl DetachedJob {
    /// Starts `cmd` on its own, writing its output to `log`.
    ///
    /// The process is put in a group of its own so stopping it takes the whole
    /// tree, and `kill_on_drop(false)` is what makes "detached" true: dropping
    /// the handle must not drag the process down with it.
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
        // tokio's `Command` has these inherently, which is why no
        // `std::os::*::CommandExt` import is needed.
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
            job: row.job,
            shell: row.shell.clone(),
            pid: row.pid as u32,
            cmd: row.cmd.clone(),
            log: PathBuf::from(&row.log),
            started_at: row.started_at,
            child: None,
        }
    }

    /// Whether the process is still there.
    ///
    /// With a handle this is exact. Without one -- a job adopted across a
    /// restart -- it is a pid lookup, which is all that survives the process
    /// that owned the handle.
    pub fn running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => pid_alive(self.pid),
        }
    }

    /// Stops it, taking the whole tree with it.
    ///
    /// Returns true when the process is no longer running, whether because we
    /// stopped it or because it had already finished.
    pub async fn stop(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            // Already gone: nothing to do, and *do not* go looking for its pid.
            // A pid is only unique until it is recycled, and signalling
            // whatever owns it now would be far worse than a stale entry.
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if let Some(pid) = child.id() {
                if kill_tree(pid).await {
                    let _ = child.wait().await;
                    return true;
                }
            }
            let stopped = child.kill().await.is_ok();
            let _ = child.wait().await;
            return stopped;
        }
        if !pid_alive(self.pid) {
            return true;
        }
        if is_self_or_ancestor(self.pid) {
            // A row written before `adopt` learned to refuse ancestors, or one
            // put there by hand. Killing the tree would kill this process, so
            // the honest answer is "I did not stop it".
            tracing::warn!(
                pid = self.pid,
                "refusing to stop a detached job that is this server's parent"
            );
            return false;
        }
        kill_tree(self.pid).await
    }

    pub fn view(&mut self) -> JobView {
        JobView {
            job: self.job,
            shell: self.shell.clone(),
            pid: self.pid,
            cmd: self.cmd.clone(),
            log: self.log.display().to_string(),
            started_at: self.started_at,
            running: self.running(),
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
///
/// Held by `SessionManager` so `cancel` and `del_session` can reach it, and
/// handed to `ToolContext` per call so the bash tools can. Everything mutable
/// lives behind one async mutex: starting, listing and stopping all touch the
/// same map, and stopping has to await the kill.
#[derive(Clone)]
pub struct DetachedRegistry {
    inner: Arc<AsyncMutex<Registry>>,
}

struct Registry {
    /// session -> its jobs, oldest first.
    jobs: HashMap<String, Vec<DetachedJob>>,
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
                next_job: 0,
                program,
                logs,
                store,
                env,
            })),
        }
    }

    /// Starts a detached command for `session` from shell `shell`, in `cwd`.
    ///
    /// The row is written before the job is handed out, so a crash between the
    /// two leaves a recorded process rather than a forgotten one -- and the
    /// next start adopts it.
    pub async fn start(
        &self,
        session: &str,
        shell: &str,
        cmd: &str,
        cwd: &Path,
    ) -> anyhow::Result<JobView> {
        let mut registry = self.inner.lock().await;
        let Some(program) = registry.program.clone() else {
            anyhow::bail!("no bash available; set [tools.bash] program");
        };
        registry.next_job += 1;
        let job = registry.next_job;
        let log = registry.logs.join(format!("{session}-{job}.log"));
        let mut launched = DetachedJob::launch(&program, cwd, cmd, &log, &registry.env)?;
        let id = registry.store.add_detached_job(&DetachedRow {
            id: 0,
            session: session.to_string(),
            shell: shell.to_string(),
            job,
            pid: launched.pid as i64,
            cmd: cmd.to_string(),
            log: log.display().to_string(),
            started_at: launched.started_at,
        })?;
        launched.id = id;
        launched.job = job;
        launched.shell = shell.to_string();
        let view = launched.view();
        registry.jobs.entry(session.to_string()).or_default().push(launched);
        Ok(view)
    }

    /// The jobs of one session, with their liveness.
    pub async fn list(&self, session: &str) -> Vec<JobView> {
        let mut registry = self.inner.lock().await;
        let Some(jobs) = registry.jobs.get_mut(session) else {
            return Vec::new();
        };
        jobs.iter_mut().map(|j| j.view()).collect()
    }

    /// Stops one job, or every job of one shell when no number is given.
    ///
    /// Stopped jobs leave the registry either way: what is left is the log
    /// file on disk, which is the record the model asked for.
    pub async fn stop(&self, session: &str, shell: &str, job: Option<u64>) -> Stopped {
        let mut registry = self.inner.lock().await;
        let (taken, kept): (Vec<DetachedJob>, Vec<DetachedJob>) = registry
            .jobs
            .remove(session)
            .unwrap_or_default()
            .into_iter()
            .partition(|j| match job {
                Some(number) => j.job == number,
                None => j.shell == shell,
            });
        let mut out = Stopped::default();
        for mut one in taken {
            out += Stopped::one(one.stop().await);
            let _ = registry.store.delete_detached_job(one.id);
        }
        if !kept.is_empty() {
            registry.jobs.insert(session.to_string(), kept);
        }
        out
    }

    /// Stops every job of a session that is being deleted.
    ///
    /// `cancel` leaves detached work alone on purpose; deleting the session is
    /// the one place that means "this work is not wanted either".
    pub async fn stop_session(&self, session: &str) -> Stopped {
        let mut registry = self.inner.lock().await;
        let jobs = registry.jobs.remove(session).unwrap_or_default();
        let mut out = Stopped::default();
        for mut one in jobs {
            out += Stopped::one(one.stop().await);
            let _ = registry.store.delete_detached_job(one.id);
        }
        out
    }

    /// Jobs still known, across every session.
    pub async fn count(&self) -> usize {
        self.inner.lock().await.jobs.values().map(Vec::len).sum()
    }

    /// Takes back the jobs a previous run left in the database.
    ///
    /// Rows whose process is gone are dropped: that is finished work, and
    /// keeping it would only make the list lie. Rows naming our own pid are
    /// dropped too -- that is a server that recorded its own launch, and
    /// adopting it would let `kill_bash` shut this process down. The same goes
    /// for any ancestor of ours, which is what a launcher that spawns the
    /// server instead of `exec`ing it leaves behind (see `is_self_or_ancestor`).
    ///
    /// The pid is only checked for liveness, never proven to be the same
    /// process, so a recycled pid could in principle be adopted. The window is
    /// narrow and the cost is bounded by `kill_bash`, which is explicit.
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
        let pids: Vec<u32> = rows.iter().map(|r| r.pid as u32).collect();
        let live = alive_pids(&pids);
        let mut adopted = 0;
        let mut registry = self.inner.lock().await;
        // Job numbers must not be reused: a job adopted from the database has
        // to keep the number the previous run gave it.
        registry.next_job = registry.next_job.max(rows.iter().map(|r| r.job).max().unwrap_or(0));
        for row in rows {
            let pid = row.pid as u32;
            // Dead rows are the common case and the cheap one to answer: no
            // walk up the parent chain for a process that is not there.
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
                     ancestors: `kill_bash` kills the tree, and the tree contains us"
                );
                let _ = registry.store.delete_detached_job(row.id);
                continue;
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

/// Kills a process tree: the pid and everything it started.
///
/// A detached command is almost always a shell running several processes, so
/// killing the leader alone would leave the real work behind. On Windows that
/// means `taskkill /T`, which only understands real Windows pids -- one more
/// reason the job has to be our own child rather than a shell background job.
///
/// The same reasoning applies to a persistent shell being shut down: its
/// children hold the write end of the pipe we read, so killing only the shell
/// leaves a reader waiting for a close that never comes.
pub(crate) async fn kill_tree(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .await;
        if matches!(&out, Ok(o) if o.status.success()) {
            return true;
        }
    }
    // Unix: the job was started in its own group, so the group is the tree.
    #[cfg(unix)]
    {
        let out = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .output()
            .await;
        if matches!(&out, Ok(o) if o.status.success()) {
            return true;
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
    }
    false
}

/// Whether a pid is currently in use.
///
/// Used for jobs adopted across a restart, where there is no handle to ask and
/// `try_wait` is not available.
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        // CSV keeps this locale-independent: the fields are always image, pid,
        // session, session number, memory -- in that order -- so the pid is
        // always the second one and no header has to be parsed.
        let out = std::process::Command::new("tasklist")
            .args(["/NH", "/FO", "CSV", "/FI", &format!("PID eq {pid}")])
            .output();
        matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|line| line
                .split(',')
                .nth(1)
                .map(|f| f.trim().trim_matches('"'))
                == Some(&pid.to_string())))
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(windows, unix)))]
    {
        false
    }
}

/// Whether `pid` is this process or one of its ancestors.
///
/// `adopt` refuses a row naming our own pid, because `kill_bash` kills the
/// whole tree and a job that is us would let the model shut the server down.
/// That check only covers a launcher that `exec`s the new server: `exec` keeps
/// the pid, so the row ends up naming us. A launcher that *spawns* the server
/// and waits -- `bash --detached "cd . && ./gongga-server"`, which is what a
/// hand-written launcher looks like -- leaves the row naming our parent, and
/// the tree under that pid still contains us. Same trap, one level up, which is
/// why the answer is the whole chain and not just the parent.
fn is_self_or_ancestor(pid: u32) -> bool {
    let me = std::process::id();
    if pid == me {
        return true;
    }
    let mut child = me;
    // Bounded because a parent chain is a handful of links and a snapshot can
    // lie: a pid recycled mid-walk must not turn this into a hang at startup.
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

    // One snapshot per hop rather than one for the whole walk: the walk is a
    // few links deep and a snapshot is a kernel call -- unlike `tasklist`, which
    // is a subprocess, and this runs on the startup path where that matters.
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

/// Which of `pids` are in use right now, answered for all of them at once.
///
/// Adoption used to ask one pid at a time, and each question is a `tasklist`
/// subprocess: a home that had run a few hundred detached jobs spent over a
/// minute walking rows of finished work before it ever bound the port, so a
/// client restarting a server saw only a timeout. Nothing about the question
/// needs to be asked per pid -- one listing answers every row in it.
///
/// A listing that cannot be read answers "none are alive", which is what the
/// per-pid version did when `tasklist` failed: the rows are then dropped as
/// finished work rather than adopted on a hunch.
fn alive_pids(pids: &[u32]) -> std::collections::HashSet<u32> {
    let wanted: std::collections::HashSet<u32> =
        pids.iter().copied().filter(|p| *p != 0).collect();
    if wanted.is_empty() {
        return wanted;
    }
    #[cfg(windows)]
    {
        // No `/FI`: the point is one call for the whole set. CSV keeps this
        // locale-independent -- image, pid, session, session number, memory --
        // so the pid is always the second field of a row.
        let out = std::process::Command::new("tasklist")
            .args(["/NH", "/FO", "CSV"])
            .output();
        let Ok(out) = out else {
            return std::collections::HashSet::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split(',').nth(1))
            .filter_map(|f| f.trim().trim_matches('"').parse::<u32>().ok())
            .filter(|pid| wanted.contains(pid))
            .collect()
    }
    #[cfg(unix)]
    {
        // `ps` prints every pid at once; the trailing `=` drops the header.
        let out = std::process::Command::new("ps")
            .args(["-e", "-o", "pid="])
            .output();
        let Ok(out) = out else {
            return std::collections::HashSet::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .filter(|pid| wanted.contains(pid))
            .collect()
    }
    #[cfg(not(any(windows, unix)))]
    {
        std::collections::HashSet::new()
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

    fn registry(dir: &Path, program: Option<PathBuf>) -> DetachedRegistry {
        let store = Store::open(&dir.join("g.db")).expect("store");
        DetachedRegistry::new(store, program, dir.join("logs"), vec![])
    }

    /// One call answers the whole set: our own pid is in it, a pid nobody can
    /// be holding is not, and asking about both costs one listing.
    #[test]
    fn liveness_is_answered_for_many_pids_at_once() {
        let me = std::process::id();
        let absent = 4_000_000u32; // above any pid a run of this suite reaches
        let live = alive_pids(&[me, absent, 0]);
        assert!(live.contains(&me), "own pid must be reported alive");
        assert!(!live.contains(&absent), "a bogus pid must not be");
        assert!(!live.contains(&0), "pid 0 is not a process");
        assert!(alive_pids(&[]).is_empty());
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
        let script = format!("sleep 1; echo done > '{}'", msys_path(&flag));

        let shell = match BashShell::spawn(&program, dir.path(), &[]) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        let cwd = shell.cwd().to_path_buf();
        let reg = registry(dir.path(), Some(program.clone()));
        let view = reg.start("main", "default", &script, &cwd).await.expect("start");
        assert!(view.running, "it is alive the moment it is started");

        // The shell it was started from is gone; the job does not notice.
        drop(shell);
        let listed = reg.list("main").await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].running);

        tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
        assert!(
            flag.exists(),
            "the detached job did not survive the shell; log says: {:?}",
            std::fs::read_to_string(&listed[0].log).unwrap_or_default()
        );
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
        let script = format!("sleep 20; echo done > '{}'", msys_path(&flag));
        let reg = registry(dir.path(), Some(program));

        let view = reg.start("main", "build", &script, dir.path()).await.expect("start");
        let stopped = reg.stop("main", "build", Some(view.job)).await;
        assert_eq!(stopped.stopped, 1, "the job should have been stopped");
        assert!(reg.list("main").await.is_empty(), "a stopped job is forgotten");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!flag.exists(), "the detached job was not stopped");
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

        // Stdio is closed on purpose: a child holding the test's stdout pipe
        // open keeps `cargo test` waiting for output long after the test ends.
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
                shell: "default".into(),
                job: 7,
                pid: held.id() as i64,
                cmd: "sleep 30".into(),
                log: dir.path().join("logs/7.log").display().to_string(),
                started_at: 0,
            })
            .expect("recording a live job");
        // A pid nobody owns: finished work, not to be adopted back.
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                shell: "default".into(),
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
        assert_eq!(listed[0].job, 7, "it keeps the number the previous run gave it");
        assert!(listed[0].running);

        // An adopted job has no handle, only a pid, and can still be stopped.
        let stopped = fresh.stop("main", "default", Some(7)).await;
        assert_eq!(stopped.stopped, 1, "an adopted job is still stoppable");
        let _ = held.kill();
        let _ = held.wait();
    }

    /// Our own pid must never be adopted: `kill_bash` would then be able to
    /// shut the server down from inside.
    #[tokio::test]
    async fn a_server_never_adopts_its_own_pid() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("g.db")).expect("store");
        store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                shell: "default".into(),
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

    /// Our own pid is the `exec` case; the parent is the spawn-and-wait case,
    /// which is what a hand-written launcher produces. Adopting it would put
    /// `taskkill /T` one call away from the server's own process tree.
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
                shell: "default".into(),
                job: 3,
                pid: parent as i64,
                cmd: "bash ./gongga-server &".into(),
                log: "x.log".into(),
                started_at: 0,
            })
            .expect("recording our parent");

        let fresh = DetachedRegistry::new(store.clone(), None, dir.path().join("logs"), vec![]);
        assert_eq!(fresh.adopt().await, 0, "a parent is not a job we may manage");
        assert!(
            store.detached_jobs().expect("rows").is_empty(),
            "the row must go, so nothing lists a job that would kill us"
        );
        assert!(fresh.list("main").await.is_empty());
    }
}
