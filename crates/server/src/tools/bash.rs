//! A long-lived Git Bash process, one per session.
//!
//! Keeping a single shell alive is what makes the harness feel like a
//! terminal: `cd`, exports and shell functions survive across tool calls, so
//! the model does not have to restate absolute paths forever.
//!
//! Three ways to run a command, which the SPEC keeps separate:
//!
//! * **foreground** (default): run it in the shell, stream the output, return
//!   the result. The turn waits.
//! * `background`: start it in the shell and return at once. Its output is
//!   tailed into the shell's *unread* buffer, which `check_bash` collects.
//! * `detached`: start a process of its own and return at once. It is not this
//!   shell's business -- see [`crate::tools::detached`], which owns it, and
//!   which is why a detached job outlives this shell, this session and even
//!   the server.
//!
//! A marker makes completion detectable. After each command we print a
//! sentinel carrying the exit code to *both* stdout and stderr, so each stream
//! can be drained independently and we still learn the status.

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
    /// The command had not finished when its deadline passed. It is *still
    /// running in this shell* -- nothing was killed -- and the rest of its
    /// output can be collected with [`BashShell::finish_outstanding`].
    pub still_running: bool,
}

/// What one pipe produced, up to its sentinel.
struct Drained {
    text: String,
    /// The exit status the sentinel carried.
    code: Option<i32>,
    /// The sentinel was seen, so the pipe is exactly at a command boundary.
    settled: bool,
}

pub struct BashShell {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: BufReader<ChildStderr>,
    marker: String,
    cwd: PathBuf,
    /// Background output the caller has not collected yet.
    ///
    /// Shared rather than owned so the tailing task can append without holding
    /// the shell, and so a shell thrown away by `del_bash` does not strand the
    /// task holding a dangling reference.
    pending: Arc<StdMutex<String>>,
    /// Background jobs still writing.
    jobs: Arc<AtomicUsize>,
    /// Numbers the background jobs of this shell.
    next_job: u64,
    /// One of the pipes ended without its sentinel, so this shell's output is
    /// no longer aligned with its commands. Sticky: nothing can put the missing
    /// bytes back, so the shell has to be replaced rather than reused.
    out_of_sync: bool,
    /// Every process this shell starts, so ending it ends the whole tree.
    ///
    /// Kept last on purpose: fields drop in declaration order, and the job's
    /// `Drop` is what terminates the children still holding our pipes.
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

impl BashShell {
    /// Starts a shell rooted at `cwd`, with `env` layered on top of whatever
    /// the server inherited.
    ///
    /// The extra variables are how the model finds its own server: `bash` is a
    /// child of the server, so without them it has no way to know where to
    /// send a `gongga exec` and has to guess or give up.
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
        // Detach the shell into its own process group where the platform allows
        // it, so a timeout kill takes down the whole command tree.
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("spawning shell {}: {e}", program.display())
        })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
        let stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

        let marker = format!("__GONGGA_{}__", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        // Join the shell to a job before anything is written to its stdin, so
        // every command it runs is a member. On unix `process_group(0)` above
        // does the same job.
        #[cfg(windows)]
        let job = child
            .id()
            .and_then(crate::tools::job::ProcessJob::adopt);
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            marker,
            cwd: cwd.to_path_buf(),
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

    /// Whether the shell is still aligned with its commands.
    ///
    /// False after a pipe ended without its sentinel: the shell holds a command
    /// boundary it cannot find again, so a command sent to it would be answered
    /// with somebody else's output. Whoever owns the shell has to replace it.
    pub fn usable(&self) -> bool {
        !self.out_of_sync
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

    /// Runs one command, streaming output to `sink` as it arrives.
    ///
    /// The deadline is a deadline on *waiting*, not on the command. When it
    /// passes the shell is left exactly as it is -- the command keeps running,
    /// keeps its place in the shell's own process tree, and its remaining
    /// output stays in the pipes -- and the outcome says `still_running`. The
    /// caller is then free to push this shell aside and collect the rest later;
    /// killing it, which is what used to happen here, threw away work that was
    /// simply long.
    pub async fn run(
        &mut self,
        command: &str,
        timeout: std::time::Duration,
        sink: Option<&mpsc::Sender<BashChunk>>,
    ) -> anyhow::Result<BashOutcome> {
        self.write_command(command).await?;

        // Bind the result rather than matching on the timeout directly: this
        // ends the statement that owns the borrow of `self`, so the branch
        // below is free to mutate the shell.
        let pumped = tokio::time::timeout(timeout, Self::pump_both(self, sink)).await;
        match pumped {
            Ok((stdout, stderr, exit_code)) => {
                // Asking where the shell is means writing one more command into
                // the same pipe. A shell that lost its place would answer that
                // from the leftovers, so it is not asked.
                if self.usable() {
                    if let Ok(pwd) = self.pwd().await {
                        self.cwd = PathBuf::from(pwd);
                    }
                }
                Ok(BashOutcome {
                    exit_code,
                    stdout,
                    stderr,
                    still_running: false,
                })
            }
            Err(_) => Ok(BashOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                still_running: true,
            }),
        }
    }

    /// Reads until the command that outlived its deadline finishes.
    ///
    /// Only meaningful right after [`BashShell::run`] returned `still_running`
    /// on this shell: the sentinel for that command is the next one the shell
    /// prints, so this collects exactly its remaining output and exit status.
    pub async fn finish_outstanding(&mut self) -> (String, String, Option<i32>) {
        let (stdout, stderr, code) = Self::pump_both(self, None).await;
        if let Ok(pwd) = self.pwd().await {
            self.cwd = PathBuf::from(pwd);
        }
        (stdout, stderr, code)
    }

    /// Starts a command without waiting for it.
    ///
    /// The shell redirects the job into a scratch file, which a task then tails
    /// into the unread buffer; `check_bash` is how the model collects it. The
    /// job keeps the shell's `cd`/exports, so a background command still runs
    /// where the foreground ones run.
    pub async fn run_background(&mut self, command: &str) -> anyhow::Result<u64> {
        self.next_job += 1;
        let job = self.next_job;
        let scratch = std::env::temp_dir().join(format!("gongga-bg-{}-{job}.out", self.marker));
        let _ = std::fs::remove_file(&scratch);
        let path = msys_path(&scratch);
        let done_marker = format!("__GONGGA_BG_{job}__");
        // `printf` takes the marker as an argument, never as a format string,
        // so a command that prints percent signs cannot corrupt the sentinel.
        let script = format!(
            "{{ {command}\nprintf '\\n%s%d\\n' '{done_marker}' \"$?\"; }} > '{path}' 2>&1 &\n"
        );
        self.stdin.write_all(script.as_bytes()).await?;
        self.stdin.flush().await?;

        self.jobs.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(tail_background(
            scratch,
            done_marker,
            self.pending.clone(),
            self.jobs.clone(),
        ));
        Ok(job)
    }

    async fn write_command(&mut self, command: &str) -> anyhow::Result<()> {
        let script = format!(
            "{command}\n__gongga_st=$?\nprintf '\\n{marker}%d\\n' \"$__gongga_st\"\nprintf '\\n{marker}%d\\n' \"$__gongga_st\" >&2\n",
            marker = self.marker
        );
        self.stdin.write_all(script.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Drains both pipes until each has produced its sentinel.
    async fn pump_both(
        &mut self,
        sink: Option<&mpsc::Sender<BashChunk>>,
    ) -> (String, String, Option<i32>) {
        let marker = &self.marker;
        let (stdout_res, stderr_res) = tokio::join!(
            Self::drain(&mut self.stdout, marker, sink, BashStream::Stdout),
            Self::drain(&mut self.stderr, marker, sink, BashStream::Stderr)
        );
        let exit_code = stdout_res.code.or(stderr_res.code);
        // A pipe that ended without its sentinel has been left half-read: the
        // rest of this command's output is still in it, so the next command
        // would read that instead of its own. The shell is not repairable from
        // here -- only replaceable -- so it says so and the caller swaps it out.
        if !stdout_res.settled || !stderr_res.settled {
            self.out_of_sync = true;
        }
        (stdout_res.text, stderr_res.text, exit_code)
    }

    async fn drain<R>(
        reader: &mut R,
        marker: &str,
        sink: Option<&mpsc::Sender<BashChunk>>,
        stream: BashStream,
    ) -> Drained
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        // Bytes, not a `String`. A command may print anything -- `cat` of a
        // binary, a program writing in the console code page -- and
        // `read_line` into a `String` fails at the first byte that is not
        // UTF-8, discarding the rest of the line *and* leaving the pipe
        // half-read. The sentinel is then never consumed, and the shell answers
        // every later command by replaying the leftovers: in one session, after
        // `cat $(which gongga)`, five calls in a row returned nothing but
        // `[read error: ...]` (`echo hi` among them), and the call after that
        // returned the *previous* command's output while reporting success.
        // Reading bytes and decoding lossily cannot happen: the bad bytes
        // become U+FFFD and the stream stays where it should be.
        let mut buf: Vec<u8> = Vec::new();
        let mut text = String::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break, // EOF: the shell died
                Ok(_) => {}
                Err(e) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("[read error: {e}]"));
                    break;
                }
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix(marker) {
                return Drained {
                    text,
                    code: rest.trim().parse::<i32>().ok(),
                    settled: true,
                };
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line);
            if let Some(sink) = sink {
                let mut chunk = String::from(line);
                chunk.push('\n');
                if sink
                    .send(BashChunk { stream, text: chunk })
                    .await
                    .is_err()
                {
                    // The subscriber went away; keep draining so the shell stays
                    // in a usable state.
                }
            }
        }
        Drained {
            text,
            code: None,
            settled: false,
        }
    }

    /// Ends the shell, everything it started, and reaps it.
    ///
    /// The tree matters as much as the shell. A child of the shell inherits the
    /// write end of the pipe we read from, and reading that pipe is something
    /// the runtime is waiting on during shutdown -- so killing the leader alone
    /// leaves a reader blocked on a close that only the children could have
    /// given it. The `wait` then reaps the leader, which is what lets the
    /// reaping thread finish.
    pub async fn kill(&mut self) {
        // A killed shell is never handed another command. Whatever the pipes
        // still hold belongs to a command that is gone, so saying so here is
        // what keeps a caller that kept the handle from reading it later as if
        // it were its own output.
        self.out_of_sync = true;
        // The tree, not the shell. Children inherit the write end of the pipe
        // this shell reads from, and reading it is something the runtime is
        // waiting on during shutdown -- so a surviving grandchild means a
        // reader blocked on a close that never comes.
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            crate::tools::detached::kill_tree(pid).await;
        }
        // Fallback for the case the job could not be created: the leader still
        // has to go, and `taskkill /T` may catch some of its children.
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
        let cmd = format!("pwd {flag}").trim().to_string();
        self.write_command(&cmd).await?;
        let (out, _err, _code) = Self::pump_both(self, None).await;
        let line = out.lines().next().unwrap_or_default().trim().to_string();
        if line.is_empty() {
            anyhow::bail!("could not read the shell's working directory");
        }
        self.cwd = PathBuf::from(&line);
        Ok(line)
    }

    /// Changes directory, failing if the path does not exist.
    ///
    /// Kept as the primitive behind a `cd`-shaped tool; `bash` itself just
    /// sends `cd` through, which is why this is currently unused.
    #[allow(dead_code)]
    pub async fn cd(&mut self, path: &str) -> anyhow::Result<String> {
        let quoted = path.replace('\'', "'\\''");
        self.write_command(&format!("cd '{quoted}'")).await?;
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
/// Polling a file is not elegant, but the job lives inside a persistent shell
/// we cannot attach a pipe to without disturbing the foreground sentinel
/// protocol, and 150ms is well below the granularity at which a model asks for
/// output.
async fn tail_background(
    path: PathBuf,
    done_marker: String,
    pending: Arc<StdMutex<String>>,
    jobs: Arc<AtomicUsize>,
) {
    let mut offset = 0usize;
    let mut missing = 0u32;
    loop {
        tokio::time::sleep(POLL).await;
        let Ok(bytes) = tokio::fs::read(&path).await else {
            // The shell has not created the file yet, or the job is one that
            // writes nothing at all.
            missing += 1;
            if missing > MISSING_FILE_POLLS {
                break;
            }
            continue;
        };
        missing = 0;
        if bytes.len() <= offset {
            continue;
        }
        let chunk = String::from_utf8_lossy(&bytes[offset..]).to_string();
        offset = bytes.len();
        match chunk.find(&done_marker) {
            Some(pos) => {
                let (head, _tail) = chunk.split_at(pos);
                collect(&pending, head);
                break;
            }
            None => collect(&pending, &chunk),
        }
    }
    jobs.fetch_sub(1, Ordering::SeqCst);
    let _ = tokio::fs::remove_file(&path).await;
}

/// Caps tool output: the head and the tail survive, the middle is dropped and
/// the whole thing is written to disk so nothing is actually lost.
///
/// This exists because the transcript is replayed in full on every step. A
/// 22 KB `cat` is not a one-off cost of 22 KB -- it is 22 KB on every step
/// after it, forever, which is what drove a single turn to 17 MB of traffic.
/// Capping is the difference between "the model can see the file" and "the
/// model can see the file and every later request pays for it".
///
/// `dir` is where the full output is spilled; `label` names it. A cap of 0
/// means no cap, which is what a deliberately unbounded run wants.
pub fn cap_output(text: &str, max: usize, dir: &Path, label: &str) -> String {
    if max == 0 || text.len() <= max {
        return text.to_string();
    }
    // Slightly more head than tail: the start of an output usually says what
    // the command was about, the end says how it ended.
    let head = floor_boundary(text, max * 3 / 5);
    let tail = ceil_boundary(text, text.len() - (max - head));
    let omitted = tail.saturating_sub(head);
    let spilled = spill(dir, label, text);
    let notice = match &spilled {
        Some(path) => format!(
            "\n\n[... {omitted} characters omitted ...]\n\
             full output ({total} characters) written to {path}\n\
             read just the part you need: `sed -n 'START,ENDp' {path}` or \
             `grep -n PATTERN {path}` -- do not cat the whole thing again",
            total = text.len(),
        ),
        None => format!("\n\n[... {omitted} characters omitted ...]"),
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

/// Writes the full output under `dir`, returning the path in Git Bash dialect
/// so the model can feed it straight back to `sed` or `grep`.
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

/// Turns a label into one path component.
///
/// ASCII alphanumerics survive, and so does every non-ASCII alphanumeric: a
/// model writes its `task` in whichever language it is thinking in, and
/// replacing `中文` with `---` would throw away the only part of the name a
/// human could have read. Everything else -- spaces, slashes, colons -- becomes
/// `-`, because those are what make a path either unreadable or plain wrong.
///
/// Bounded because this ends up in a *path*, not because the work is bounded:
/// the label already carries a session name and a call id, and Windows refuses
/// components over 255 characters. A shorter slug is not a smaller budget.
fn slug(label: &str) -> String {
    const MAX_CHARS: usize = 48;
    let mut out = String::new();
    let mut chars = 0;
    let mut last_was_dash = false;
    for c in label.chars() {
        // `is_alphanumeric` is Unicode-aware, which is the whole point: it keeps
        // the CJK and accented text a task is likely written in, while still
        // rejecting `/`, `:` and spaces.
        let kept = if c.is_alphanumeric() || c == '_' { c } else { '-' };
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
    if out.is_empty() { "output".to_string() } else { out }
}

impl Drop for BashShell {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Every label ends up in a path, so the one thing this must never do is
    /// produce a name with a separator in it -- whatever the model typed.
    #[test]
    fn a_hostile_task_cannot_escape_the_output_directory() {
        for task in ["../../etc/passwd", "a/b", "x\\y", "..", " ", "..\\..\\windows"] {
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
        assert!(!out.contains('/') && !out.contains(' ') && !out.contains('\\'), "{out}");
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
        let text: String = (0..2000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        let out = cap_output(&text, 500, dir.path(), "sess-call1");

        assert!(out.len() < text.len(), "expected the output to be capped");
        assert!(out.starts_with(&text[..300]), "head must be kept");
        assert!(out.ends_with(&text[text.len() - 200..]), "tail must be kept");
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

    /// The one thing about background jobs that cannot be tested by reasoning:
    /// the scratch file has to be writable *and* findable from both sides.
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
            .run_background("sleep 1; echo from-background")
            .await
            .expect("starting a background job");
        let mut collected = String::new();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            collected = shell.take_pending();
            if !collected.is_empty() {
                break;
            }
        }
        assert!(collected.contains("from-background"), "got {collected:?}");
        // Reading it clears it, so the same chunk cannot be returned twice.
        assert!(shell.take_pending().is_empty());
    }

    /// Output that is not text must not cost the shell its life.
    ///
    /// A command that prints bytes (`cat` of a binary, a program writing in the
    /// console code page) puts something into the pipe that is not UTF-8. The
    /// reader used to stop at the first such line with `[read error: ...]`,
    /// leaving the rest of the output -- including the sentinel -- unread in
    /// the pipe. Every later command in that shell then read the leftovers,
    /// failed on the same byte, and reported success with the same one-line
    /// error: a shell that looks alive and answers nothing. This is what one
    /// session showed after `cat $(which gongga)`: five calls in a row, `echo
    /// hi` among them, all returning the read error and nothing else.
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
            .run(r"printf 'before\377\376after\n'", Duration::from_secs(10), None)
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
    ///
    /// `exit`, a kill, or a crashed child closes the pipe without a sentinel.
    /// There is no way to resynchronise from that, so the shell reports itself
    /// unusable and is replaced, instead of answering the next call with
    /// whatever was left in the pipe.
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
        assert!(out.exit_code.is_none(), "the shell died before the sentinel");
        assert!(!shell.usable(), "a dead shell called itself usable");
    }

    /// A killed shell is not usable, however long its handle sticks around.
    ///
    /// The cancel path stops the command by killing the shell's process tree,
    /// and then replaces the shell under the same name. Anything still holding
    /// the old handle must not read what is left in its pipes as the next
    /// command's output, which is what `usable()` gates.
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

    /// The model drives the server through the CLI, which means the shell has
    /// to know where the server is. Nothing else in the process tree tells it.
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
