//! The channels: a program a session started, and what nobody has read of it since.
//!
//! See `docs/notes/server-channel.md`.
//!
//! # Invariant
//!
//! A channel's program is not this server's child: it is a relay's
//! (`crate::relay`), which is what makes the channel outlive a `cancel` and a
//! restart. This process reads and writes two files in the channel's directory and
//! never holds a pipe of the program's own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::store::{ChannelRow, Store};
use crate::tools::spec;

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

use std::sync::Arc;

/// How long one look at a channel waits before looking again.
///
/// # Invariant
///
/// A poll, not a budget: the call's own `wait` is what ends the wait, and this is
/// only how often the file is re-read while it lasts.
const LOOK: Duration = Duration::from_millis(25);

/// How long a channel's first `status` may take. A relay that cannot say who it
/// started is a start that failed, not a call that waits forever.
const START_DEADLINE: Duration = Duration::from_secs(20);

/// Every channel this server holds, by the name each session calls it.
pub struct Channels {
    store: Store,
    /// `<home>/channels`: one directory per channel, named by an opaque id.
    root: PathBuf,
    /// The program a relay is started as: this server, in another mode.
    server: PathBuf,
    /// What `[channel.<name>]` says each known name is.
    grammar: HashMap<String, spec::ChannelSpec>,
    /// Where a finished channel is announced.
    notify: Notifier,
}

impl Channels {
    pub fn new(
        store: Store,
        root: PathBuf,
        server: PathBuf,
        grammar: HashMap<String, spec::ChannelSpec>,
        notify: Notifier,
    ) -> Self {
        Self {
            store,
            root,
            server,
            grammar,
            notify,
        }
    }

    /// The channels of one home: its directory, its config's names, and the program
    /// a relay is started as.
    ///
    /// # Invariant
    ///
    /// The one way a `Channels` is built, so every process that may speak to a
    /// channel agrees on where they live and which names it knows.
    pub fn for_home(cfg: &crate::config::Config, store: Store, notify: Notifier) -> Self {
        let server = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("gongga-server"));
        Self::new(
            store,
            cfg.home.join("channels"),
            server,
            cfg.tools.channels.clone().into_iter().collect(),
            notify,
        )
    }

    /// A `Channels` nobody speaks to: for a context that reads one answer and writes
    /// one row, and for a build whose config names no program.
    pub fn without_words(store: Store, root: PathBuf) -> Self {
        Self::new(
            store,
            root,
            PathBuf::from("gongga-server"),
            HashMap::new(),
            Arc::new(|_| {}),
        )
    }

    /// What the config says a name is, for a caller that has to route before it starts.
    pub fn kind_of(&self, name: &str) -> spec::Kind {
        self.grammar_of(name).kind
    }

    /// What the config says a name is; a name it does not mention is raw.
    fn grammar_of(&self, name: &str) -> spec::ChannelSpec {
        self.grammar.get(name).cloned().unwrap_or_default()
    }

    /// Starts a program as a channel, or says why it could not.
    ///
    /// # Invariant
    ///
    /// The name is a key of this server's namespace and the directory is where the
    /// bytes live: nothing renames a directory, and nothing reads a channel by a
    /// path on disk. See [`SPEC`](SPEC.md), "Tool".
    pub async fn start(
        &self,
        session: &str,
        name: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        id: Option<String>,
    ) -> Result<ChannelRow, String> {
        let grammar = self.grammar_of(name);
        // A cmd is answered by this server: there is no program to start, and saying
        // so is what keeps one name from meaning two things.
        if grammar.kind == spec::Kind::Cmd {
            return Err(format!(
                "{name:?} is a cmd, not a program: `new` answers it through the command \
                 layer, and it has no channel of its own"
            ));
        }
        let path = match id {
            Some(path) => {
                if self
                    .store
                    .channel_of(session, &path)
                    .map_err(quiet)?
                    .is_some()
                {
                    return Err(format!(
                        "channel {path:?} is in use; `check {path}` speaks to it, and \
                         `del_job {path}` ends it"
                    ));
                }
                path
            }
            None => self.auto_name(session, name)?,
        };
        let dir = self
            .root
            .join(format!("ch-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {dir:?}: {e}"))?;
        let words = if args.is_empty() {
            grammar.spawn.clone()
        } else {
            args.to_vec()
        };
        let written = serde_json::to_vec(&crate::relay::Words {
            program: name.to_string(),
            args: words,
            cwd: cwd.to_path_buf(),
            env: env.to_vec(),
        })
        .map_err(|e| format!("could not describe {name:?}: {e}"))?;
        std::fs::write(dir.join("cmd"), written)
            .map_err(|e| format!("could not write {:?}: {e}", dir.join("cmd")))?;
        // The command file exists before the relay looks at it, so its first line is
        // never missed.
        std::fs::write(dir.join("in"), b"").map_err(|e| format!("could not write `in`: {e}"))?;
        let log = dir.join("relay.log");
        let relay = crate::tools::detached::DetachedJob::launch_program(
            &self.server,
            &["--relay".to_string(), dir.display().to_string()],
            &self.root,
            &log,
            env,
        )
        .map_err(|e| format!("could not start a relay for {name:?}: {e}"))?;
        // The relay is no part of this server's tree: it is what keeps the program
        // alive after a `cancel`, and after this process is gone.
        crate::tools::job::release(relay.pid);
        let status = match self.await_status(&dir).await {
            Some(status) => status,
            None => {
                return Err(format!(
                    "a relay for {name:?} started but never said who it started; its own \
                     words are in {log:?}"
                ));
            }
        };
        self.store
            .add_channel(session, &path, name, &dir, status.pid)
            .map_err(quiet)?;
        let row = self
            .store
            .channel_of(session, &path)
            .map_err(quiet)?
            .ok_or_else(|| format!("channel {path:?} was written and cannot be read back"))?;
        tracing::info!(channel = %path, dir = %dir.display(), pid = status.pid, "channel started");
        Ok(row)
    }

    /// The name a call that states none gets: this session's, this program's, and the
    /// first number under them that is free.
    fn auto_name(&self, session: &str, name: &str) -> Result<String, String> {
        let taken = self.store.channels_of(session).map_err(quiet)?;
        for number in 1..=u32::MAX {
            // No leading `/`: a path that does begins with the shell's root, and a
            // caller on Git Bash has it rewritten into this machine's own path before
            // the line ever reaches this server. Measured: `/main/bash/1` arrived as
            // `C:/SoftWare/git/Git/main/bash/1`.
            let candidate = format!("{session}/{name}/{number}");
            if !taken.iter().any(|row| row.path == candidate) {
                return Ok(candidate);
            }
        }
        Err(format!("no free name for {name:?} in this session"))
    }

    /// Waits for the relay's first `status`, which says who it started.
    async fn await_status(&self, dir: &Path) -> Option<crate::relay::Status> {
        let deadline = Instant::now() + START_DEADLINE;
        loop {
            if let Some(status) = read_status(dir) {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Speaks to a channel: writes the input, then reads until the marker, the end of
    /// the program, or the end of the wait.
    ///
    /// # Invariant
    ///
    /// Waiting is the call's, never the grammar's: `wait` absent is this server's
    /// ceiling and `wait: 0` is no wait at all. See [`SPEC`](SPEC.md), "Tool".
    pub async fn talk(
        &self,
        session: &str,
        id: &str,
        input: Option<&str>,
        wait: Duration,
        until: Option<&str>,
    ) -> Result<Outcome, String> {
        let row = self.row_of(session, id)?;
        let dir = PathBuf::from(&row.dir);
        let grammar = self.grammar_of(&row.program);
        // A sentinel exists only where a reply has to be told apart from the next one:
        // it is generated per write, and only for a write. A read alone has nothing to
        // frame, so a grammar whose marker is a sentinel contributes no marker to it.
        let sentinel = if input.is_some() && grammar.until.as_deref().is_some_and(has_sentinel) {
            Some(new_sentinel())
        } else {
            None
        };
        if let Some(input) = input {
            let spelled = match &grammar.write {
                Some(spelled) => spelled
                    .replace("{{input}}", input)
                    .replace("{{sentinel}}", sentinel.as_deref().unwrap_or_default()),
                // No spelling: the input is one line, and a shell reads a line as a
                // command. This is what "raw" means for a name the table never named.
                None => format!("{input}\n"),
            };
            let mut command = String::from("input ");
            command.push_str(&crate::base64::encode(spelled.as_bytes()));
            command.push('\n');
            append(&dir.join("in"), &command)?;
        }
        let marker = match until {
            Some(marker) => Some(marker.to_string()),
            None => match (&sentinel, &grammar.until) {
                (Some(sentinel), Some(spelled)) => Some(spelled.replace("{{sentinel}}", sentinel)),
                (None, Some(spelled)) if !has_sentinel(spelled) => Some(spelled.clone()),
                _ => None,
            },
        };
        let deadline = Instant::now() + wait;
        loop {
            let looked = look(&dir, row.upto, marker.as_deref(), &grammar);
            let ended = read_status(&dir).and_then(|status| status.exit);
            if looked.matched || ended.is_some() || wait.is_zero() {
                return self.answer(
                    row,
                    looked.window,
                    looked.matched,
                    looked.upto,
                    looked.exit.or(ended),
                );
            }
            if Instant::now() >= deadline {
                // Nothing arrived within the wait: what is there is the answer, and the
                // cursor moves past it so the next call is not told the same thing.
                return self.answer(row, looked.window, false, looked.upto, None);
            }
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Records what a call read, and answers with it.
    fn answer(
        &self,
        row: ChannelRow,
        output: String,
        matched: bool,
        read_to: u64,
        exit: Option<i32>,
    ) -> Result<Outcome, String> {
        self.store
            .set_channel_upto(row.id, read_to)
            .map_err(quiet)?;
        if let Some(code) = exit {
            self.store.finish_channel(row.id, code).map_err(quiet)?;
        }
        Ok(Outcome {
            id: row.path.clone(),
            output,
            time: Duration::from_millis(
                (now_millis() - row.started_at)
                    .max(0)
                    .try_into()
                    .unwrap_or(0),
            ),
            matched,
            exit,
        })
    }

    /// One channel of one session, as this server has it.
    fn row_of(&self, session: &str, id: &str) -> Result<ChannelRow, String> {
        self.store
            .channel_of(session, id)
            .map_err(quiet)?
            .ok_or_else(|| {
                format!("no channel {id:?} in this session; `ls_job` lists the ones it has")
            })
    }

    /// Every channel one session has, oldest name first.
    pub fn list(&self, session: &str) -> Result<Vec<Value>, String> {
        let rows = self.store.channels_of(session).map_err(quiet)?;
        Ok(rows
            .iter()
            .map(|row| {
                let dir = PathBuf::from(&row.dir);
                let unread = look(&dir, row.upto, None, &spec::ChannelSpec::default())
                    .window
                    .len();
                json!({
                    "id": row.path,
                    "session": row.session,
                    "program": row.program,
                    "pid": row.pid,
                    "dir": row.dir,
                    "started_at": row.started_at,
                    "ended_at": match row.ended_at {
                        Some(at) => json!(at),
                        None => Value::Null,
                    },
                    "exit": match row.exit {
                        Some(code) => json!(code),
                        None => Value::Null,
                    },
                    "unread": unread,
                })
            })
            .collect())
    }

    /// The channel a session calls this, when it has one and it is still running.
    pub fn live(&self, session: &str, id: &str) -> Option<ChannelRow> {
        self.store
            .channel_of(session, id)
            .ok()
            .flatten()
            .filter(|row| row.exit.is_none())
    }

    /// Ends a channel, and forgets it.
    ///
    /// # Invariant
    ///
    /// `kill` in the command file, then the row: a channel whose program is still
    /// writing must not look gone while it is not.
    pub async fn stop(&self, session: &str, id: &str) -> Result<bool, String> {
        let row = self.row_of(session, id)?;
        let dir = PathBuf::from(&row.dir);
        if row.exit.is_some() {
            self.store.delete_channel(session, id).map_err(quiet)?;
            return Ok(false);
        }
        append(&dir.join("in"), "kill\n")?;
        let deadline = Instant::now() + START_DEADLINE;
        while read_status(&dir).and_then(|status| status.exit).is_none() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "channel {id:?} was told to end and did not; its relay log is in \
                     {:?}",
                    dir.join("relay.log")
                ));
            }
            tokio::time::sleep(LOOK).await;
        }
        self.store.delete_channel(session, id).map_err(quiet)?;
        Ok(true)
    }

    /// Gives a channel another name: one key of this table, and nothing on disk.
    pub fn rename(&self, session: &str, id: &str, new_id: &str) -> Result<(), String> {
        if self
            .store
            .channel_of(session, new_id)
            .map_err(quiet)?
            .is_some()
        {
            return Err(format!("channel {new_id:?} is in use"));
        }
        self.row_of(session, id)?;
        self.store
            .rename_channel(session, id, new_id)
            .map_err(quiet)
    }

    /// Ends every channel of one session, for `del_session`.
    pub async fn stop_session(&self, session: &str) -> usize {
        let rows = match self.store.channels_of(session) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(%session, "could not read a session's channels: {e}");
                return 0;
            }
        };
        let mut stopped = 0;
        for row in rows {
            let dir = PathBuf::from(&row.dir);
            if row.exit.is_none() && append(&dir.join("in"), "kill\n").is_ok() {
                stopped += 1;
            }
            let _ = self.store.delete_channel(session, &row.path);
        }
        stopped
    }

    /// Watches every live channel, and tells a session when its program ends.
    ///
    /// # Invariant
    ///
    /// A notice is one more `check`: it reads the same window, so a session is never
    /// told bytes it already read, and never loses the tail. The stop token is what
    /// keeps a `cancel` from announcing what it just ended.
    pub fn watch(self: &Arc<Self>, stop: CancellationToken) -> tokio::task::JoinHandle<()> {
        let channels = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(LOOK) => {}
                }
                let live = match channels.store.live_channels() {
                    Ok(live) => live,
                    Err(e) => {
                        tracing::warn!("could not read the live channels: {e}");
                        continue;
                    }
                };
                for row in live {
                    let dir = PathBuf::from(&row.dir);
                    let Some(status) = read_status(&dir) else {
                        continue;
                    };
                    let Some(exit) = status.exit else {
                        continue;
                    };
                    if channels.store.finish_channel(row.id, exit).is_err() {
                        continue;
                    }
                    // The window is not taken here: what the program said is read by
                    // whoever asks, and a notice that consumed it would be the one way
                    // to lose it.
                    let looked = look(&dir, row.upto, None, &spec::ChannelSpec::default());
                    let output = looked.window;
                    (channels.notify)(ChannelEnd {
                        session: row.session.clone(),
                        id: row.path.clone(),
                        output,
                        time: Duration::from_millis(
                            (now_millis() - row.started_at)
                                .max(0)
                                .try_into()
                                .unwrap_or(0),
                        ),
                        exit: Some(exit),
                    });
                }
            }
        })
    }
}

/// One look at what a channel has said beyond `upto`.
struct Looked {
    /// The window nobody has read, cut at the marker when it is there.
    window: String,
    matched: bool,
    /// Where the next read starts: past the marker, and past the digits its own tail
    /// carried when the grammar puts the status there.
    upto: u64,
    /// The status the marker's tail carried, when it carried one.
    exit: Option<i32>,
}

/// Reads the file once, by hand: the relay may still be appending, so the answer is
/// "what is there now", and the offset comes back so the caller can record it.
fn look(dir: &Path, upto: u64, marker: Option<&str>, grammar: &spec::ChannelSpec) -> Looked {
    let path = dir.join("out");
    let bytes = std::fs::read(&path).unwrap_or_default();
    let from = usize::try_from(upto).unwrap_or(0).min(bytes.len());
    let fresh = String::from_utf8_lossy(&bytes[from..]).to_string();
    let Some(marker) = marker else {
        return Looked {
            window: fresh,
            matched: false,
            upto: bytes.len() as u64,
            exit: None,
        };
    };
    let Some(at) = fresh.find(marker) else {
        return Looked {
            window: fresh,
            matched: false,
            upto: bytes.len() as u64,
            exit: None,
        };
    };
    let mut window = fresh[..at].to_string();
    trim_newline(&mut window);
    let after = &fresh[at + marker.len()..];
    // The grammar may say the marker's own tail is the status: `__GONGGA__0`.
    let (exit, eaten) = if grammar.exit_from_marker {
        let digits: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        let leading = after.len() - after.trim_start().len();
        let mut eaten = leading + digits.len();
        // The rest of that line is the marker's, not the program's.
        if after[eaten..].starts_with("\r\n") {
            eaten += 2;
        } else if after[eaten..].starts_with('\n') {
            eaten += 1;
        }
        (digits.parse::<i32>().ok(), eaten)
    } else {
        (None, 0)
    };
    Looked {
        window,
        matched: true,
        upto: (from + at + marker.len() + eaten) as u64,
        exit,
    }
}

/// Whether a marker is spelled with the sentinel this server generates per write.
fn has_sentinel(spelled: &str) -> bool {
    spelled.contains("{{sentinel}}")
}

/// A marker no program prints by accident.
fn new_sentinel() -> String {
    format!("__gongga-{}__", uuid::Uuid::new_v4().simple())
}

/// A trailing newline the program's own echo left, not part of what it said.
fn trim_newline(window: &mut String) {
    if window.ends_with("\r\n") {
        window.truncate(window.len() - 2);
    } else if window.ends_with('\n') {
        window.truncate(window.len() - 1);
    }
}

/// What the relay last said about its program, when it has said anything.
fn read_status(dir: &Path) -> Option<crate::relay::Status> {
    let bytes = std::fs::read(dir.join("status")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Appends one command to a channel's command file.
fn append(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("could not open {path:?}: {e}"))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("could not write {path:?}: {e}"))?;
    file.flush()
        .map_err(|e| format!("could not flush {path:?}: {e}"))
}

/// A store failure, in the words a tool answer carries.
fn quiet(e: anyhow::Error) -> String {
    format!("this home's database: {e}")
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}
