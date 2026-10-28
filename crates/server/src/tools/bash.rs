//! One long-lived Git Bash per session; `cd` and exports survive across calls.
//!
//! See `docs/notes/server-tools-bash.md`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

/// How often a background job's scratch file is re-read.
const POLL: Duration = Duration::from_millis(150);
/// Polls without the scratch file ever appearing before giving up (~6s).
const MISSING_FILE_POLLS: u32 = 40;

/// Where a chunk of tool output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashStream {
    Stdout,
    Stderr,
}

/// A chunk of streamed output.
#[derive(Debug, Clone)]
pub struct BashChunk {
    pub stream: BashStream,
    pub text: String,
}

/// The settled result of one command.
#[derive(Debug, Clone)]
pub struct BashOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The deadline passed with the command still running in this shell; nothing was killed.
    pub still_running: bool,
}

/// One pipe's resumable state, kept in the shell: a cancelled wait loses neither
/// output nor a seen sentinel.
#[derive(Default)]
struct DrainState {
    text: String,
    /// A line read in progress; unlike a future-local buffer, this survives timeout.
    buf: Vec<u8>,
    code: Option<i32>,
    settled: bool,
}

pub struct BashShell {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: BufReader<ChildStderr>,
    stdout_state: DrainState,
    stderr_state: DrainState,
    marker: String,
    cwd: PathBuf,
    /// A view of `cwd` that can be read while this shell's own lock is held.
    /// See `docs/notes/server-tools-bash.md`.
    cwd_cell: Arc<StdMutex<PathBuf>>,
    /// Background output the caller has not collected yet, shared so a tail can append.
    pending: Arc<StdMutex<String>>,
    /// Background jobs still writing.
    jobs: Arc<AtomicUsize>,
    /// Numbers the background jobs of this shell.
    next_job: u64,
    /// A pipe ended without its sentinel, so output no longer lines up with commands.
    out_of_sync: bool,
    /// Every process the shell starts, so ending it ends the tree; kept last for `Drop`.
    #[cfg(windows)]
    job: Option<crate::tools::job::ProcessJob>,
}

/// Hands a piece of background output to whoever eventually asks for it.
fn collect(pending: &StdMutex<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Ok(mut guard) = pending.lock() {
        guard.push_str(text);
    }
}

/// Renders a native path the way Git Bash spells it: `C:\Temp` -> `/c/Temp`.
pub(crate) fn msys_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("/{}{}", drive.to_ascii_lowercase(), &text[2..])
        }
        _ => text.to_string(),
    }
}

/// Quotes one word for the shell, including embedded single quotes.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl BashShell {
    /// Starts a shell rooted at `cwd`, with `env` layered on top of the server's own.
    pub fn spawn(program: &Path, cwd: &Path, env: &[(String, String)]) -> anyhow::Result<Self> {
        let mut cmd = Command::new(program);
        cmd.arg("--norc")
            .arg("--noprofile")
            .current_dir(cwd)
            .env("TERM", "dumb")
            .env("PS1", "")
            .env("BASH_SILENCE_DEPRECATION_WARNING", "1");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Its own process group, so a timeout kill takes the whole command tree.
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning shell {}: {e}", program.display()))?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
        let stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

        let marker = format!(
            "__GONGGA_{}__",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        // Join the job before anything reaches stdin, so every command is a member.
        #[cfg(windows)]
        let job = child.id().and_then(crate::tools::job::ProcessJob::adopt);
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            stdout_state: DrainState::default(),
            stderr_state: DrainState::default(),
            marker,
            cwd: cwd.to_path_buf(),
            cwd_cell: Arc::new(StdMutex::new(cwd.to_path_buf())),
            pending: Arc::new(StdMutex::new(String::new())),
            jobs: Arc::new(AtomicUsize::new(0)),
            next_job: 0,
            out_of_sync: false,
            #[cfg(windows)]
            job,
        })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The same working directory, readable without this shell's lock.
    ///
    /// # Invariant
    ///
    /// The cell outlives every borrow of the shell, so a reader cannot be made
    /// to wait on the command currently holding it.
    pub fn cwd_cell(&self) -> Arc<StdMutex<PathBuf>> {
        self.cwd_cell.clone()
    }

    /// Records where the shell now is, for readers holding the cell too.
    fn set_cwd(&mut self, path: PathBuf) {
        self.cwd = path.clone();
        if let Ok(mut guard) = self.cwd_cell.lock() {
            *guard = path;
        }
    }

    /// Whether the shell is still aligned with its commands; false means replace it.
    pub fn usable(&self) -> bool {
        !self.out_of_sync
    }

    /// The shell process's own id, for a teardown that must not take its lock.
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or_default()
    }

    /// Bytes of background output waiting to be read.
    pub fn pending_len(&self) -> usize {
        self.pending.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Background jobs that have not finished.
    pub fn background_jobs(&self) -> usize {
        self.jobs.load(Ordering::SeqCst)
    }

    /// Hands back everything unread and marks it read.
    pub fn take_pending(&mut self) -> String {
        self.pending
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    /// Takes foreground output an interrupted wait left, keeping each pipe's
    /// sentinel state for `finish_outstanding`.
    pub fn take_partial_output(&mut self) -> (String, String) {
        (
            std::mem::take(&mut self.stdout_state.text),
            std::mem::take(&mut self.stderr_state.text),
        )
    }

    /// Runs one command, streaming to `sink`; the deadline covers waiting only.
    pub async fn run(
        &mut self,
        command: &str,
        timeout: std::time::Duration,
        sink: Option<&mpsc::Sender<BashChunk>>,
    ) -> anyhow::Result<BashOutcome> {
        self.write_command(command).await?;

        // Bound first, so the statement owning the borrow of `self` has ended.
        let pumped = tokio::time::timeout(timeout, Self::pump_both(self, sink)).await;
        match pumped {
            Ok((stdout, stderr, exit_code)) => {
                // A shell that lost its place is not asked: `pwd` would read the
                // leftovers. See `docs/notes/server-tools-bash.md`.
                if self.usable() {
                    match tokio::time::timeout(timeout, self.pwd()).await {
                        Ok(Ok(pwd)) => self.set_cwd(PathBuf::from(pwd)),
                        Ok(Err(_)) => {}
                        Err(_) => self.out_of_sync = true,
                    }
                }
                Ok(BashOutcome {
                    exit_code,
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

    /// Reads until the command that outlived its deadline finishes, as `run` left it.
    pub async fn finish_outstanding(&mut self) -> (String, String, Option<i32>) {
        // Retired: an extra `pwd` would be an unbounded command. The pump gives only
        // what the timed-out call left.
        Self::pump_both(self, None).await
    }

    /// Starts a command without waiting; its output is tailed into the unread buffer.
    pub async fn run_background(&mut self, command: &str) -> anyhow::Result<u64> {
        self.next_job += 1;
        let job = self.next_job;
        let scratch = std::env::temp_dir().join(format!("gongga-bg-{}-{job}.out", self.marker));
        let pid_file = scratch.with_extension("pid");
        let _ = std::fs::remove_file(&scratch);
        let _ = std::fs::remove_file(&pid_file);
        let path = shell_quote(&msys_path(&scratch));
        let pid_path = shell_quote(&msys_path(&pid_file));
        // Git Bash's `$!` is an MSYS pid; Windows process APIs need its winpid.
        let pid = if cfg!(windows) {
            "$(</proc/$!/winpid)"
        } else {
            "$!"
        };
        // Unguessable, like the foreground marker: a job that prints this line
        // would otherwise end its own output early.
        let done_marker = format!(
            "__GONGGA_BG_{}_{job}__",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        // The wrapper, not the job, writes the sentinel: see
        // `docs/notes/server-tools-bash.md`.
        let script = format!(
            "{{ (\n{command}\n); command printf '\\n%s%d\\n' '{done_marker}' \"$?\"; }} > {path} 2>&1 &\ncommand printf '%s\\n' \"{pid}\" > {pid_path}\n"
        );
        self.stdin.write_all(script.as_bytes()).await?;
        self.stdin.flush().await?;

        self.jobs.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(tail_background(
            scratch,
            pid_file,
            done_marker,
            self.pending.clone(),
            self.jobs.clone(),
        ));
        Ok(job)
    }

    async fn write_command(&mut self, command: &str) -> anyhow::Result<()> {
        let script = format!(
            "{command}\n__gongga_st=$?\ncommand printf '\\n{marker}%d\\n' \"$__gongga_st\"\ncommand printf '\\n{marker}%d\\n' \"$__gongga_st\" >&2\n",
            marker = self.marker
        );
        self.stdin.write_all(script.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Drains both pipes to their sentinels; progress lives on `self`, so a timeout
    /// can resume the exact same place.
    async fn pump_both(
        &mut self,
        sink: Option<&mpsc::Sender<BashChunk>>,
    ) -> (String, String, Option<i32>) {
        let marker = &self.marker;
        tokio::join!(
            Self::drain(
                &mut self.stdout,
                &mut self.stdout_state,
                marker,
                sink,
                BashStream::Stdout
            ),
            Self::drain(
                &mut self.stderr,
                &mut self.stderr_state,
                marker,
                sink,
                BashStream::Stderr
            )
        );
        let exit_code = self.stdout_state.code.or(self.stderr_state.code);
        // Half-read pipe: the rest would be the next command's output, so flag the shell.
        if !self.stdout_state.settled || !self.stderr_state.settled {
            self.out_of_sync = true;
        }
        let stdout = std::mem::take(&mut self.stdout_state.text);
        let stderr = std::mem::take(&mut self.stderr_state.text);
        self.stdout_state = DrainState::default();
        self.stderr_state = DrainState::default();
        (stdout, stderr, exit_code)
    }

    async fn drain<R>(
        reader: &mut R,
        state: &mut DrainState,
        marker: &str,
        sink: Option<&mpsc::Sender<BashChunk>>,
        stream: BashStream,
    ) where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        // The other pipe may have timed out after this one finished: a settled pipe
        // must not wait for a second sentinel.
        if state.settled {
            return;
        }
        // Bytes, not `String`: a bad byte must not cost the sentinel. `state.buf`
        // keeps a half line across a cancelled future.
        loop {
            match reader.read_until(b'\n', &mut state.buf).await {
                Ok(0) => break, // EOF: the shell died
                Ok(_) => {}
                Err(e) => {
                    if !state.text.is_empty() {
                        state.text.push('\n');
                    }
                    state.text.push_str(&format!("[read error: {e}]"));
                    break;
                }
            }
            let line = String::from_utf8_lossy(&state.buf);
            let line = line
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string();
            if let Some(rest) = line.strip_prefix(marker) {
                state.buf.clear();
                state.code = rest.trim().parse::<i32>().ok();
                state.settled = true;
                return;
            }
            if !state.text.is_empty() {
                state.text.push('\n');
            }
            state.text.push_str(&line);
            state.buf.clear();
            if let Some(sink) = sink {
                let mut chunk = line;
                chunk.push('\n');
                if sink
                    .send(BashChunk {
                        stream,
                        text: chunk,
                    })
                    .await
                    .is_err()
                {
                    // The subscriber went away; keep draining so the shell stays
                    // in a usable state.
                }
            }
        }
    }

    /// Ends the shell and everything it started, then reaps the leader.
    pub async fn kill(&mut self) {
        // Kept-handle callers must not read what is left in the pipes as their own output.
        self.out_of_sync = true;
        // The tree, not the shell: a surviving grandchild holds the pipe open at shutdown.
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            crate::tools::detached::kill_tree(pid).await;
        }
        // Fallback when the job could not be created: the leader still has to go.
        #[cfg(windows)]
        if self.job.is_none() {
            if let Some(pid) = self.child.id() {
                crate::tools::detached::kill_tree(pid).await;
            }
        }
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }

    /// Resolves the shell's current directory to a native path.
    pub async fn pwd(&mut self) -> anyhow::Result<String> {
        // Git Bash's `pwd` reports an MSYS path; `-W` reports the Windows one.
        let flag = if cfg!(windows) { "-W" } else { "" };
        let cmd = format!("command pwd {flag}").trim().to_string();
        self.write_command(&cmd).await?;
        let (out, _err, _code) = Self::pump_both(self, None).await;
        let line = out.lines().next().unwrap_or_default().trim().to_string();
        if line.is_empty() {
            anyhow::bail!("could not read the shell's working directory");
        }
        self.set_cwd(PathBuf::from(&line));
        Ok(line)
    }

    /// Changes directory, failing if the path does not exist; unused, `bash` sends `cd`.
    #[allow(dead_code)]
    pub async fn cd(&mut self, path: &str) -> anyhow::Result<String> {
        let quoted = shell_quote(path);
        self.write_command(&format!("cd {quoted}")).await?;
        let (out, err, code) = Self::pump_both(self, None).await;
        if code != Some(0) {
            anyhow::bail!(
                "cannot cd to {path:?}: {}",
                if err.is_empty() { out } else { err }
            );
        }
        self.pwd().await
    }
}

/// Follows a background job's scratch file into the shell's unread buffer.
///
/// See `docs/notes/server-tools-bash.md`.
async fn tail_background(
    path: PathBuf,
    pid_file: PathBuf,
    done_marker: String,
    pending: Arc<StdMutex<String>>,
    jobs: Arc<AtomicUsize>,
) {
    let mut offset = 0usize;
    let mut missing = 0u32;
    let mut carry = Vec::new();
    let mut settled = false;
    loop {
        tokio::time::sleep(POLL).await;
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                missing = 0;
                if bytes.len() > offset {
                    settled =
                        push_background_chunk(&mut carry, &bytes[offset..], &done_marker, &pending);
                    offset = bytes.len();
                    if settled {
                        break;
                    }
                }
            }
            Err(_) => {
                // The shell has not created the file yet, or the job is one that
                // writes nothing at all.
                missing += 1;
                if missing > MISSING_FILE_POLLS {
                    break;
                }
            }
        }
        // The wrapper's sentinel covers `exit`, `exec` and normal termination; this is
        // the fallback for a killed wrapper.
        if let Ok(pid) = tokio::fs::read_to_string(&pid_file).await {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                if !pid_alive(pid) {
                    break;
                }
            }
        }
    }
    if !settled && !carry.is_empty() {
        collect(&pending, &String::from_utf8_lossy(&carry));
    }
    jobs.fetch_sub(1, Ordering::SeqCst);
    let _ = tokio::fs::remove_file(&path).await;
    let _ = tokio::fs::remove_file(&pid_file).await;
}

/// Adds new bytes, holding back a partial sentinel; true once the whole sentinel
/// has been consumed.
fn push_background_chunk(
    carry: &mut Vec<u8>,
    bytes: &[u8],
    marker: &str,
    pending: &StdMutex<String>,
) -> bool {
    carry.extend_from_slice(bytes);
    let marker = marker.as_bytes();
    if let Some(pos) = carry
        .windows(marker.len())
        .position(|window| window == marker)
    {
        collect(pending, &String::from_utf8_lossy(&carry[..pos]));
        carry.clear();
        return true;
    }
    let keep = (1..=marker.len().min(carry.len()))
        .rev()
        .find(|&len| carry.ends_with(&marker[..len]))
        .unwrap_or(0);
    let emit = carry.len() - keep;
    if emit > 0 {
        collect(pending, &String::from_utf8_lossy(&carry[..emit]));
        carry.drain(..emit);
    }
    false
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    pid != 0 && unsafe { kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(process);
        alive
    }
}

#[cfg(not(any(unix, windows)))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// Caps tool output: the head and the tail survive, the rest is spilled to disk.
///
/// See `docs/notes/server-tools-bash.md`.
pub fn cap_output(text: &str, max: usize, dir: &Path, label: &str) -> String {
    if max == 0 || text.len() <= max {
        return text.to_string();
    }
    // More head than tail: the start says what it was, the end says how it went.
    let head = floor_boundary(text, max * 3 / 5);
    let tail = ceil_boundary(text, text.len() - (max - head));
    let omitted = tail.saturating_sub(head);
    let spilled = spill(dir, label, text);
    let omitted = omitted.to_string();
    let notice = match &spilled {
        Some(path) => crate::prompts::shared().render(
            "output_cap",
            &[
                ("omitted", omitted.as_str()),
                ("total", &text.len().to_string()),
                ("path", path.as_str()),
            ],
        ),
        None => crate::prompts::shared().render("output_omitted", &[("omitted", omitted.as_str())]),
    };
    format!("{}{}{}", &text[..head], notice, &text[tail..])
}

/// Rounds `i` down to a UTF-8 boundary, so slicing never splits a character.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Rounds `i` up to a UTF-8 boundary.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Writes the full output under `dir`, returning the path in Git Bash dialect.
fn spill(dir: &Path, label: &str, text: &str) -> Option<String> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let safe = slug(label);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{safe}-{stamp}.out"));
    if std::fs::write(&path, text).is_err() {
        return None;
    }
    Some(msys_path(&path))
}

/// Turns a label into one path component, keeping Unicode alphanumerics.
///
/// See `docs/notes/server-tools-bash.md`.
fn slug(label: &str) -> String {
    const MAX_CHARS: usize = 48;
    let mut out = String::new();
    let mut chars = 0;
    let mut last_was_dash = false;
    for c in label.chars() {
        // Unicode-aware on purpose: it keeps CJK and accents and still drops `/`, `:` and spaces.
        let kept = if c.is_alphanumeric() || c == '_' {
            c
        } else {
            '-'
        };
        if kept == '-' {
            // Collapse runs, and never start a name with one.
            if last_was_dash || out.is_empty() {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        if chars >= MAX_CHARS {
            break;
        }
        chars += 1;
        out.push(kept);
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "output".to_string()
    } else {
        out
    }
}

impl Drop for BashShell {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testbed::wait_until;

    /// Long enough for a shell to start on a loaded machine, short enough that
    /// a hang is reported instead of inherited.
    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn short_output_is_returned_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let out = cap_output("hello", 8_000, dir.path(), "s-call");
        assert_eq!(out, "hello");
    }

    /// The name has to survive a real `task`: paths and punctuation become
    /// dashes, but a Chinese description stays readable.
    #[test]
    fn a_task_becomes_a_readable_file_name() {
        assert_eq!(slug("cargo build"), "cargo-build");
        assert_eq!(slug("跑测试 验证中文"), "跑测试-验证中文");
        assert_eq!(slug("../etc/passwd"), "etc-passwd");
        assert_eq!(slug("   "), "output");
        assert_eq!(slug(""), "output");
        // Runs collapse rather than turning into `a----b`.
        assert_eq!(slug("a  ..  b"), "a-b");
    }

    /// A label ends up in a path, so it must never carry a separator.
    #[test]
    fn a_hostile_task_cannot_escape_the_output_directory() {
        for task in [
            "../../etc/passwd",
            "a/b",
            "x\\y",
            "..",
            " ",
            "..\\..\\windows",
        ] {
            let safe = slug(task);
            assert!(!safe.contains('/'), "{task} -> {safe}");
            assert!(!safe.contains('\\'), "{task} -> {safe}");
            assert!(!safe.contains(':'), "{task} -> {safe}");
            assert!(!safe.is_empty(), "{task} produced nothing");
        }
    }

    #[test]
    fn a_long_task_is_bounded_and_still_valid() {
        let out = slug(&"步骤".repeat(100));
        assert!(out.chars().count() <= 48, "{out}");
        // Still usable as a path component: no separators, no spaces.
        assert!(
            !out.contains('/') && !out.contains(' ') && !out.contains('\\'),
            "{out}"
        );
    }

    #[test]
    fn a_cap_of_zero_means_no_cap() {
        let dir = tempfile::tempdir().unwrap();
        let text = "x".repeat(50);
        assert_eq!(cap_output(&text, 0, dir.path(), "s-call"), text);
    }

    #[test]
    fn long_output_is_capped_and_spilled() {
        let dir = tempfile::tempdir().unwrap();
        let text: String = (0..2000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = cap_output(&text, 500, dir.path(), "sess-call1");

        assert!(out.len() < text.len(), "expected the output to be capped");
        assert!(out.starts_with(&text[..300]), "head must be kept");
        assert!(
            out.ends_with(&text[text.len() - 200..]),
            "tail must be kept"
        );
        assert!(out.contains("characters omitted"));

        // The full output is on disk, in a path the model can use directly.
        let path = out
            .split("written to ")
            .nth(1)
            .and_then(|rest| rest.split('\n').next())
            .expect("the notice must name the spill file");
        assert!(path.starts_with('/'), "Git Bash dialect, got {path}");
        let win = path.trim_start_matches('/');
        let on_disk = format!("{}:{}", win[..1].to_uppercase(), &win[1..]);
        let saved = std::fs::read_to_string(on_disk.replace('/', "\\")).unwrap();
        assert_eq!(saved, text, "the spilled file must hold the whole output");
    }

    /// Slicing at an arbitrary byte offset must never split a multi-byte
    /// character, or this panics instead of capping.
    #[test]
    fn capping_respects_utf8_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let text: String = "é".repeat(5000);
        let out = cap_output(&text, 400, dir.path(), "s-call");
        assert!(out.len() < text.len());
        assert!(out.contains("characters omitted"));
    }

    #[test]
    fn markers_are_unique_per_shell() {
        // Two shells must never share a sentinel, or output could be truncated.
        let a = uuid::Uuid::new_v4().simple().to_string();
        let b = uuid::Uuid::new_v4().simple().to_string();
        assert_ne!(a, b);
    }

    #[test]
    fn windows_paths_become_msys_paths() {
        // Git Bash does not understand `C:\...`, and a background job's
        // scratch file has to be named in the shell's dialect.
        assert_eq!(msys_path(Path::new(r"C:\Temp\x.out")), "/c/Temp/x.out");
        assert_eq!(msys_path(Path::new("/tmp/x.out")), "/tmp/x.out");
    }

    #[test]
    fn shell_quoting_keeps_single_quotes_in_a_path() {
        assert_eq!(shell_quote("/tmp/it's done"), "'/tmp/it'\\''s done'");
    }

    #[test]
    fn background_sentinel_can_span_reads() {
        let pending = StdMutex::new(String::new());
        let mut carry = Vec::new();
        assert!(!push_background_chunk(
            &mut carry,
            b"before\n__GONG",
            "__GONGGA_BG_1__",
            &pending
        ));
        assert_eq!(pending.lock().unwrap().as_str(), "before\n");
        assert!(push_background_chunk(
            &mut carry,
            b"GA_BG_1__0\n",
            "__GONGGA_BG_1__",
            &pending
        ));
        assert_eq!(pending.lock().unwrap().as_str(), "before\n");
    }

    /// Only a real run shows it: the scratch file must be writable and findable.
    #[tokio::test]
    async fn background_output_waits_for_check_bash() {
        let Some(program) = crate::config::detect_git_bash() else {
            // No Git Bash here: the foreground/background split is still
            // exercised by the unit tests above.
            return;
        };
        let mut shell = match BashShell::spawn(&program, &std::env::temp_dir(), &[]) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        shell
            .run_background("sleep 0.2; echo from-background")
            .await
            .expect("starting a background job");
        let mut collected = String::new();
        wait_until("output from the background job", WAIT, || {
            collected = shell.take_pending();
            !collected.is_empty()
        })
        .await;
        assert!(collected.contains("from-background"), "got {collected:?}");
        // Reading it clears it, so the same chunk cannot be returned twice.
        assert!(shell.take_pending().is_empty());
        shell.kill().await;
    }

    #[tokio::test]
    async fn a_timeout_keeps_partial_output_for_exactly_one_completion() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let mut shell = BashShell::spawn(&program, &std::env::temp_dir(), &[]).unwrap();
        let early = shell
            .run(
                "printf 'before\\npart'; sleep 1; printf 'ial\\nafter\\n'",
                Duration::from_millis(500),
                None,
            )
            .await
            .unwrap();
        assert!(early.still_running);
        assert_eq!(early.stdout.trim_end(), "before");
        assert!(early.stderr.is_empty());

        let (stdout, stderr, code) = tokio::time::timeout(WAIT, shell.finish_outstanding())
            .await
            .expect("the outstanding command must finish");
        assert_eq!(code, Some(0));
        assert_eq!(stdout.trim_end(), "partial\nafter");
        assert_eq!(stderr, "");
        assert!(!stdout.contains("before"), "partial output was repeated");
        shell.kill().await;
    }

    #[tokio::test]
    async fn background_jobs_finish_without_a_user_command_sentinel() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let mut shell = BashShell::spawn(&program, &std::env::temp_dir(), &[]).unwrap();
        for command in [
            "printf 'exit-done\\n'; exit 7",
            "exec printf 'exec-done\\n'",
            "printf 'kill-done\\n'; kill -9 \"$BASHPID\"",
        ] {
            shell.run_background(command).await.unwrap();
        }
        let mut collected = String::new();
        wait_until(
            "all background jobs without normal command completion",
            WAIT,
            || {
                collected.push_str(&shell.take_pending());
                shell.background_jobs() == 0
            },
        )
        .await;
        assert_eq!(shell.background_jobs(), 0, "jobs never settled");
        for expected in ["exit-done", "exec-done", "kill-done"] {
            assert!(
                collected.contains(expected),
                "missing {expected}: {collected:?}"
            );
        }
        assert!(!collected.contains("__GONGGA_BG_"), "{collected:?}");
        shell.kill().await;
    }

    #[tokio::test]
    async fn a_dead_background_wrapper_finishes_without_a_sentinel() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("job.out");
        let pid_file = dir.path().join("job.pid");
        tokio::fs::write(&output, b"last output").await.unwrap();
        let mut child = Command::new(program)
            .arg("-c")
            .arg("exit 7")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        child.wait().await.unwrap();
        tokio::fs::write(&pid_file, pid.to_string()).await.unwrap();
        let pending = Arc::new(StdMutex::new(String::new()));
        let jobs = Arc::new(AtomicUsize::new(1));
        tokio::time::timeout(
            WAIT,
            tail_background(
                output.clone(),
                pid_file.clone(),
                "__GONGGA_BG_TEST__".to_string(),
                pending.clone(),
                jobs.clone(),
            ),
        )
        .await
        .expect("a dead process must finish its tail");
        assert_eq!(jobs.load(Ordering::SeqCst), 0);
        assert_eq!(pending.lock().unwrap().as_str(), "last output");
        assert!(!output.exists() && !pid_file.exists());
    }

    #[tokio::test]
    async fn protocol_builtins_cannot_be_replaced_by_shell_functions() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let mut shell = BashShell::spawn(&program, &std::env::temp_dir(), &[]).unwrap();
        let outcome = tokio::time::timeout(
            WAIT,
            shell.run(
                "printf() { command printf 'hijacked\\n'; }; pwd() { command printf '/hijacked\\n'; }; echo safe",
                Duration::from_secs(1),
                None,
            ),
        )
        .await
        .expect("internal pwd must not hang")
        .unwrap();
        assert_eq!(outcome.stdout.trim_end(), "safe");
        assert!(shell.usable());
        assert_ne!(shell.cwd(), Path::new("/hijacked"));
        shell.kill().await;
    }

    #[tokio::test]
    async fn cd_accepts_a_single_quote_in_the_path() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let quoted = dir.path().join("single'quote");
        std::fs::create_dir(&quoted).unwrap();
        let mut shell = BashShell::spawn(&program, dir.path(), &[]).unwrap();
        shell.cd(quoted.to_str().unwrap()).await.unwrap();
        assert_eq!(shell.cwd(), quoted);
        shell.kill().await;
    }

    /// Output that is not text must not cost the shell its life.
    ///
    /// See `docs/notes/server-tools-bash.md`.
    #[tokio::test]
    async fn binary_output_does_not_deafen_the_shell() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let mut shell = match BashShell::spawn(&program, &std::env::temp_dir(), &[]) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        let noisy = shell
            .run(
                r"printf 'before\377\376after\n'",
                Duration::from_secs(10),
                None,
            )
            .await
            .expect("running printf with raw bytes");
        let after = shell
            .run("echo still-alive", Duration::from_secs(10), None)
            .await
            .expect("running echo after binary output");
        shell.kill().await;

        assert!(
            after.stdout.contains("still-alive"),
            "the shell went deaf after binary output: {:?}",
            after.stdout
        );
        assert!(!after.stdout.contains("read error"), "{:?}", after.stdout);
        // The command that printed the bytes still reports, and its text
        // survives around the bytes that could not be decoded.
        assert!(noisy.stdout.contains("before"), "{:?}", noisy.stdout);
        assert!(noisy.stdout.contains("after"), "{:?}", noisy.stdout);
    }

    /// A shell that ended under a command must not be handed the next one.
    #[tokio::test]
    async fn a_shell_that_ended_is_not_reusable() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let mut shell = match BashShell::spawn(&program, &std::env::temp_dir(), &[]) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        let out = shell
            .run("exit 3", Duration::from_secs(10), None)
            .await
            .expect("running exit");
        assert!(
            out.exit_code.is_none(),
            "the shell died before the sentinel"
        );
        assert!(!shell.usable(), "a dead shell called itself usable");
    }

    /// A killed shell is not usable, however long its handle sticks around.
    #[tokio::test]
    async fn a_killed_shell_reports_itself_unusable() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let mut shell = match BashShell::spawn(&program, &std::env::temp_dir(), &[]) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        assert!(shell.usable());
        shell.kill().await;
        assert!(!shell.usable(), "a killed shell called itself usable");
    }

    /// The shell has to know where the server is; nothing else in the tree tells it.
    #[tokio::test]
    async fn spawn_hands_the_shell_its_variables() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let env = vec![("GONGGA_SERVER".to_string(), "127.0.0.1:9711".to_string())];
        let mut shell = match BashShell::spawn(&program, &std::env::temp_dir(), &env) {
            Ok(s) => s,
            Err(e) => panic!("spawning {}: {e}", program.display()),
        };
        let out = shell
            .run("echo $GONGGA_SERVER", Duration::from_secs(10), None)
            .await
            .expect("running echo")
            .stdout;
        assert_eq!(out.trim(), "127.0.0.1:9711");
    }
}
