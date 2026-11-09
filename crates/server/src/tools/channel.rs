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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::remote::Endpoint;
use crate::remote::transport::Runner;
use crate::store::{ChannelRow, Store};
use crate::tools::spec;

/// What one call of `new` or `check` answers: [`SPEC`](SPEC.md), "Tool".
///
/// # Invariant
///
/// `output` is the window nobody has read: taking it advances the cursor.
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

    /// A call's answer, capped like every tool's output.
    ///
    /// # Invariant
    ///
    /// One builder for the call and for the notice.
    pub fn result(&self, max_output_chars: usize, output_dir: &std::path::Path) -> Value {
        let mut answer = self.to_json();
        if let Some(window) = answer.get("output").and_then(Value::as_str) {
            let capped = crate::tools::spill::cap_output(
                crate::prompts::shared(),
                window,
                max_output_chars,
                Some(output_dir),
                &self.id,
            );
            answer["output"] = json!(capped);
        }
        answer
    }
}

/// What a channel owes the session waiting on it: an end, or the marker a call asked
/// for while the program runs on.
#[derive(Debug, Clone)]
pub struct ChannelEnd {
    pub session: String,
    pub id: String,
    pub output: String,
    pub time: Duration,
    /// `None` while the program runs: the case of a channel that only matched.
    pub exit: Option<i32>,
    /// Whether the marker a call asked for is what brought this on.
    pub matched: bool,
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

/// How long a channel's first `status` may take, before the start is a failure.
const START_DEADLINE: Duration = Duration::from_secs(20);

/// Every channel this server holds.
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
    /// The peers a call may name, as this machine's remotes file lists them.
    remotes: HashMap<String, Endpoint>,
    /// One connection per peer, kept because a channel is read on every look.
    ///
    /// # Invariant
    ///
    /// The server's own, not a session's: a channel outlives the session's shells,
    /// and the relay it reads runs on the peer whether this process is here or not.
    runners: tokio::sync::Mutex<HashMap<String, Arc<Runner>>>,
    /// The channels a call is inside right now.
    ///
    /// # Invariant
    ///
    /// A call answers with what it saw; the watcher answers what a session was waiting
    /// for. Both would be one end told twice, so the watcher skips what a call holds.
    /// In memory, because a call is only ever in this process.
    attached: std::sync::Mutex<HashSet<i64>>,
}

impl Channels {
    pub fn new(
        store: Store,
        root: PathBuf,
        server: PathBuf,
        grammar: HashMap<String, spec::ChannelSpec>,
        notify: Notifier,
        remotes: HashMap<String, Endpoint>,
    ) -> Self {
        Self {
            store,
            root,
            server,
            grammar,
            notify,
            remotes,
            runners: tokio::sync::Mutex::new(HashMap::new()),
            attached: std::sync::Mutex::new(HashSet::new()),
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
            remotes_of(cfg),
        )
    }

    /// A `Channels` nobody speaks to: for a context that reads one answer, writes one
    /// row.
    ///
    /// # Invariant
    ///
    /// The same peers as the server's own: a subscription's action calls `new` like
    /// any other caller, and a name it cannot resolve would be a name nobody can.
    pub fn without_words(cfg: &crate::config::Config, store: Store, root: PathBuf) -> Self {
        Self::new(
            store,
            root,
            PathBuf::from("gongga-server"),
            HashMap::new(),
            Arc::new(|_| {}),
            remotes_of(cfg),
        )
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
        remote: Option<&str>,
        cwd: &Path,
        env: &[(String, String)],
        id: Option<String>,
    ) -> Result<ChannelRow, String> {
        let grammar = self.grammar_of(name);
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
                         `del_channel {path}` ends it"
                    ));
                }
                path
            }
            None => self.auto_name(session, name)?,
        };
        let words = crate::relay::Words {
            program: name.to_string(),
            args: if args.is_empty() {
                grammar.spawn.clone()
            } else {
                args.to_vec()
            },
            cwd: cwd.to_path_buf(),
            env: env.to_vec(),
        };
        let started = match remote {
            // A program there gets that machine's environment, not this one's.
            Some(machine) => {
                let there = crate::relay::Words {
                    env: Vec::new(),
                    ..words
                };
                self.start_there(machine, &there).await
            }
            None => self.start_here(&words).await,
        }?;
        let (place, pid) = started;
        self.store
            .add_channel(session, &path, name, remote, &place.dir(), pid)
            .map_err(quiet)?;
        let row = self
            .store
            .channel_of(session, &path)
            .map_err(quiet)?
            .ok_or_else(|| format!("channel {path:?} was written and cannot be read back"))?;
        tracing::info!(channel = %path, dir = %place.dir(), pid, "channel started");
        Ok(row)
    }

    /// A channel on this machine: its own directory, and a relay started beside us.
    async fn start_here(&self, words: &crate::relay::Words) -> Result<(Where, u32), String> {
        let dir = self
            .root
            .join(format!("ch-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {dir:?}: {e}"))?;
        let written = serde_json::to_vec(words)
            .map_err(|e| format!("could not describe {:?}: {e}", words.program))?;
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
            &words.env,
        )
        .map_err(|e| format!("could not start a relay for {:?}: {e}", words.program))?;
        // The relay is no part of this tree: it keeps the program alive after us.
        crate::tools::job::release(relay.pid);
        let place = Where::Here(dir);
        let pid = self
            .await_status(&place, &log.display().to_string())
            .await?;
        Ok((place, pid))
    }

    /// A channel on a peer: it places its own directory, and its relay outlives us.
    ///
    /// # Invariant
    ///
    /// A directory the call names is the endpoint's to accept: one it does not have is
    /// an error here, never a quiet substitution of a directory this machine has.
    async fn start_there(
        &self,
        machine: &str,
        words: &crate::relay::Words,
    ) -> Result<(Where, u32), String> {
        let runner = self.runner_for(machine).await?;
        // `.` is the call naming no directory: the program then runs where the peer's
        // own runner stands.
        let named = words.cwd != Path::new(".");
        if named {
            let asked = words.cwd.display().to_string();
            let there = runner
                .stat(&asked)
                .await
                .map_err(|e| format!("{machine}: {e}"))?;
            if !there.is_dir {
                return Err(format!("{machine} has no directory {asked:?}"));
            }
        }
        let written = serde_json::to_vec(words)
            .map_err(|e| format!("could not describe {:?}: {e}", words.program))?;
        let cwd = named.then(|| words.cwd.display().to_string());
        let started = runner
            .relay(cwd.as_deref(), &written)
            .await
            .map_err(|e| format!("could not start a channel on {machine}: {e}"))?;
        if started.dir.is_empty() {
            return Err(format!("{machine} started a relay and named no directory"));
        }
        let place = Where::There {
            runner,
            dir: started.dir,
        };
        let pid = self.await_status(&place, &started.log).await?;
        Ok((place, pid))
    }

    /// The connection a peer answers on, started once and kept.
    ///
    /// # Invariant
    ///
    /// One per peer name, and a connection that has ended is not reused: a peer that
    /// was restarted must not answer with the silence of the run before it.
    async fn runner_for(&self, machine: &str) -> Result<Arc<Runner>, String> {
        let mut runners = self.runners.lock().await;
        if let Some(runner) = runners.get(machine)
            && runner.connected()
        {
            return Ok(Arc::clone(runner));
        }
        let Some(endpoint) = self.remotes.get(machine) else {
            let mut known: Vec<&str> = self.remotes.keys().map(String::as_str).collect();
            known.sort_unstable();
            return Err(format!(
                "no remote named {machine:?}; this machine's remotes are {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        };
        let spawned = crate::remote::pick::runner(endpoint, endpoint.answer_timeout())
            .await
            .map_err(|e| format!("{e}"))?;
        let runner = Arc::new(spawned);
        runners.insert(machine.to_string(), Arc::clone(&runner));
        Ok(runner)
    }

    /// Where one channel's files are, for a call that has its row.
    async fn place_of(&self, row: &ChannelRow) -> Result<Where, String> {
        match &row.remote {
            None => Ok(Where::Here(PathBuf::from(&row.dir))),
            Some(machine) => Ok(Where::There {
                runner: self.runner_for(machine).await?,
                dir: row.dir.clone(),
            }),
        }
    }

    /// The name a call that states none gets: this session's, this program's, and the
    /// first number under them that is free.
    fn auto_name(&self, session: &str, name: &str) -> Result<String, String> {
        let taken = self.store.channels_of(session).map_err(quiet)?;
        for number in 1..=u32::MAX {
            // No leading `/`: Git Bash rewrites such a word into this machine's path.
            let candidate = format!("{session}/{name}/{number}");
            if !taken.iter().any(|row| row.path == candidate) {
                return Ok(candidate);
            }
        }
        Err(format!("no free name for {name:?} in this session"))
    }

    /// Waits for the relay's first `status`, which says who it started.
    async fn await_status(&self, place: &Where, log: &str) -> Result<u32, String> {
        let deadline = Instant::now() + START_DEADLINE;
        loop {
            if let Some(status) = place.status().await {
                return Ok(status.pid);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "a relay started but never said who it started; its own words are in {log}"
                ));
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
        // Whoever is inside the channel is the one that tells the session: the watcher
        // skips what a call holds.
        let _held = self.attach(row.id);
        let place = self.place_of(&row).await?;
        let grammar = self.grammar_of(&row.program);
        // `time` is what this call cost: from its own write, or from the last one.
        let mut since = row.wrote_at.unwrap_or(row.started_at);
        // A sentinel is generated per write, and only for a write: a read alone has
        // nothing to frame.
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
                // No spelling: one line, which is what "raw" means for an unknown name.
                None => format!("{input}\n"),
            };
            let mut command = String::from("input ");
            command.push_str(&crate::base64::encode(spelled.as_bytes()));
            command.push('\n');
            place.append("in", &command).await?;
            // This call's own clock starts at its write, which is what `time` is.
            let at = now_millis();
            self.store
                .set_channel_wrote(session, id, at)
                .map_err(quiet)?;
            since = at;
        }
        let marker = match until {
            Some(marker) => Some(marker.to_string()),
            None => match (&sentinel, &grammar.until) {
                (Some(sentinel), Some(spelled)) => Some(spelled.replace("{{sentinel}}", sentinel)),
                (None, Some(spelled)) if !has_sentinel(spelled) => Some(spelled.clone()),
                _ => None,
            },
        };
        // The flag says a caller waits here, for what. It outlives this call only if
        // the call gives up first.
        self.store
            .set_channel_wait(session, id, marker.as_deref().unwrap_or(""))
            .map_err(quiet)?;
        let deadline = Instant::now() + wait;
        loop {
            let fresh = place.read_from("out", row.upto).await?;
            let looked = look(&fresh, row.upto, marker.as_deref(), &grammar);
            let ended = place.status().await.and_then(|status| status.exit);
            if looked.matched || ended.is_some() || wait.is_zero() {
                // Two statuses, not one fact: the command's, and the program's ending.
                return self.answer(
                    row,
                    looked.window,
                    looked.matched,
                    looked.upto,
                    looked.exit.or(ended),
                    ended,
                    since,
                );
            }
            if Instant::now() >= deadline {
                // Nothing arrived: what is there is the answer, and the cursor moves.
                return self.answer(row, looked.window, false, looked.upto, None, ended, since);
            }
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Records what a call read, and answers with it.
    ///
    /// # Invariant
    ///
    /// `exit` is what this call's command said about itself; `gone` is the program being
    /// gone. Only `gone` ends the channel: a framed command inside a persistent shell
    /// reports a status on every call, and a channel that read that as its own end would
    /// answer one command and refuse the next.
    #[allow(clippy::too_many_arguments)]
    fn answer(
        &self,
        row: ChannelRow,
        output: String,
        matched: bool,
        read_to: u64,
        exit: Option<i32>,
        gone: Option<i32>,
        since: i64,
    ) -> Result<Outcome, String> {
        self.store
            .set_channel_upto(row.id, read_to)
            .map_err(quiet)?;
        if let Some(code) = gone {
            self.store.finish_channel(row.id, code).map_err(quiet)?;
        }
        if matched || gone.is_some() {
            // What it waited for is in the answer: the flag goes, so the watcher cannot
            // repeat it.
            self.store.take_channel_wait(row.id).map_err(quiet)?;
        }
        Ok(Outcome {
            id: row.path.clone(),
            output,
            time: Duration::from_millis((now_millis() - since).max(0).try_into().unwrap_or(0)),
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
                format!("no channel {id:?} in this session; `ls_channel` lists the ones it has")
            })
    }

    /// Every channel one session has, oldest name first.
    ///
    /// # Invariant
    ///
    /// A row whose machine cannot be reached is still listed: it is this table's, and
    /// what it has said is the one thing that is `null` when nobody can ask.
    pub async fn list(&self, session: &str) -> Result<Vec<Value>, String> {
        let rows = self.store.channels_of(session).map_err(quiet)?;
        let mut listed = Vec::with_capacity(rows.len());
        for row in &rows {
            let unread = match self.place_of(row).await {
                Ok(place) => place
                    .read_from("out", row.upto)
                    .await
                    .map(|fresh| {
                        look(&fresh, row.upto, None, &spec::ChannelSpec::default())
                            .window
                            .len()
                    })
                    .map(|unread| json!(unread))
                    .unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            listed.push(json!({
                    "id": row.path,
                    "session": row.session,
                    "program": row.program,
                    // Which machine it runs on, so a reader knows where the work is.
                    "remote": match &row.remote {
                        Some(name) => json!(name),
                        None => Value::Null,
                    },
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
            }));
        }
        Ok(listed)
    }

    /// Ends a channel, and forgets it.
    ///
    /// # Invariant
    ///
    /// `kill` in the command file, then the row: a channel whose program is still
    /// writing must not look gone while it is not.
    pub async fn stop(&self, session: &str, id: &str) -> Result<bool, String> {
        let row = self.row_of(session, id)?;
        if row.exit.is_some() {
            self.store.delete_channel(session, id).map_err(quiet)?;
            return Ok(false);
        }
        let place = self.place_of(&row).await?;
        place.append("in", "kill\n").await?;
        let deadline = Instant::now() + START_DEADLINE;
        while place
            .status()
            .await
            .and_then(|status| status.exit)
            .is_none()
        {
            if Instant::now() >= deadline {
                return Err(format!(
                    "channel {id:?} was told to end and did not; its relay log is in \
                     {:?}",
                    place.file("relay.log")
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
            let alive = match self.place_of(&row).await {
                Ok(place) => place.append("in", "kill\n").await.is_ok(),
                Err(e) => {
                    tracing::warn!(channel = %row.path, "{e}");
                    false
                }
            };
            if row.exit.is_none() && alive {
                stopped += 1;
            }
            let _ = self.store.delete_channel(session, &row.path);
        }
        stopped
    }

    /// Marks a channel as one a call is inside, until the guard drops.
    fn attach(&self, id: i64) -> Attached<'_> {
        self.attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id);
        Attached {
            held: &self.attached,
            id,
        }
    }

    /// Whether a call is inside this channel right now.
    fn is_attached(&self, id: i64) -> bool {
        self.attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
    }

    /// Watches every live channel, and tells a session what it is waiting for.
    ///
    /// # Invariant
    ///
    /// A notice is one more `check`: it reads the same window, so a session is never
    /// told bytes it already read and never loses the tail. The stop token keeps a
    /// `cancel` from announcing what it just ended.
    ///
    /// Only a flagged channel speaks, and once: `new`/`check` set it, the call clears
    /// it when it answers, and the claim lets the watcher speak for one no call is in.
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
                    // Nobody is waiting on it: however it ends, it owes no notice.
                    let Some(marker) = row.wait_marker.clone() else {
                        continue;
                    };
                    // A call inside it answers its session with what it saw; saying it
                    // again here is the double notice.
                    if channels.is_attached(row.id) {
                        continue;
                    }
                    let Ok(place) = channels.place_of(&row).await else {
                        continue;
                    };
                    let ended = place.status().await.and_then(|status| status.exit);
                    // The window is not taken here: a notice that consumed it would be
                    // the one way to lose it.
                    let fresh = place.read_from("out", row.upto).await.unwrap_or_default();
                    let grammar = channels.grammar_of(&row.program);
                    let looked = look(
                        &fresh,
                        row.upto,
                        (!marker.is_empty()).then_some(marker.as_str()),
                        &grammar,
                    );
                    if ended.is_none() && !looked.matched {
                        continue;
                    }
                    // The flag is the token: whoever clears it is the one that speaks.
                    if !channels.store.take_channel_wait(row.id).unwrap_or(false) {
                        continue;
                    }
                    if let Some(exit) = ended {
                        // The claim above is what makes this once; moving the row is
                        // another writer's to do, and a refusal here is no failure.
                        let _ = channels.store.finish_channel(row.id, exit);
                    }
                    // A notice is a read: it carries the window, so the cursor
                    // moves with it.
                    let _ = channels.store.set_channel_upto(row.id, looked.upto);
                    (channels.notify)(ChannelEnd {
                        session: row.session.clone(),
                        id: row.path.clone(),
                        output: looked.window,
                        time: Duration::from_millis(
                            (now_millis() - row.started_at)
                                .max(0)
                                .try_into()
                                .unwrap_or(0),
                        ),
                        exit: ended,
                        matched: looked.matched,
                    });
                }
            }
        })
    }
}

/// A call's hold on one channel, released when the call answers.
struct Attached<'a> {
    held: &'a std::sync::Mutex<HashSet<i64>>,
    id: i64,
}

impl Drop for Attached<'_> {
    fn drop(&mut self) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// The peers this home's config names, by the name a call gives.
fn remotes_of(cfg: &crate::config::Config) -> HashMap<String, Endpoint> {
    crate::remotes::load(cfg.endpoint.answer_timeout)
        .into_iter()
        .map(|remote| (remote.name, remote.endpoint))
        .collect()
}

/// Where one channel's files are: a directory this machine has, or one a peer has.
///
/// # Invariant
///
/// A channel's directory is the machine's own business. The relay that writes it was
/// started there, so this process reads `out`, `status` and `in` through one connection
/// and never names a peer path it was not answered. See `docs/notes/server-channel.md`.
enum Where {
    Here(PathBuf),
    There { runner: Arc<Runner>, dir: String },
}

impl Where {
    /// The directory, as a row and a message spell it.
    fn dir(&self) -> String {
        match self {
            Self::Here(dir) => dir.display().to_string(),
            Self::There { dir, .. } => dir.clone(),
        }
    }

    /// One file of the channel's, by name, in the spelling its machine opens it by.
    fn file(&self, name: &str) -> String {
        match self {
            Self::Here(dir) => dir.join(name).display().to_string(),
            Self::There { dir, .. } => format!("{}/{name}", dir.trim_end_matches('/')),
        }
    }

    /// What one file holds from `from` on, or why it could not be read.
    ///
    /// # Invariant
    ///
    /// Every look asks only for what it has not read: a peer answers one chunk at a
    /// time, and a channel's `out` grows for as long as its program runs.
    async fn read_from(&self, name: &str, from: u64) -> Result<Vec<u8>, String> {
        match self {
            Self::Here(_) => {
                let path = self.file(name);
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("could not read {path:?}: {e}"))?;
                let start = usize::try_from(from).unwrap_or(0).min(bytes.len());
                Ok(bytes[start..].to_vec())
            }
            Self::There { runner, .. } => {
                let path = self.file(name);
                let mut bytes = Vec::new();
                let mut offset = from;
                loop {
                    let chunk = runner
                        .read(&path, offset)
                        .await
                        .map_err(|e| e.to_string())?;
                    let fresh = chunk.data.len();
                    bytes.extend_from_slice(&chunk.data);
                    // The endpoint's `eof` is "the file has nothing more"; so is an
                    // empty chunk, from a peer that raced the file's growth.
                    if chunk.eof || fresh == 0 {
                        return Ok(bytes);
                    }
                    offset = chunk.next;
                }
            }
        }
    }

    /// What the relay last said about its program, when it has said anything.
    async fn status(&self) -> Option<crate::relay::Status> {
        let bytes = self.read_from("status", 0).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Appends one command to the channel's command file.
    async fn append(&self, name: &str, line: &str) -> Result<(), String> {
        match self {
            Self::Here(_) => append_here(&self.file(name), line),
            Self::There { runner, .. } => {
                let path = self.file(name);
                runner
                    .append(&path, line.as_bytes())
                    .await
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// One look at what a channel has said beyond `upto`.
struct Looked {
    /// The window nobody has read, cut at the marker when it is there.
    window: String,
    matched: bool,
    /// Where the next read starts: past the marker, and past the status its tail held.
    upto: u64,
    /// The status the marker's tail carried, when it carried one.
    exit: Option<i32>,
}

/// One look at `fresh`, the bytes a channel said beyond `upto`.
///
/// # Invariant
///
/// The relay may still be appending, so this is what had arrived when it was read; what
/// it is read *from* is the cursor, because a look asks the machine for the tail only.
fn look(fresh: &[u8], upto: u64, marker: Option<&str>, grammar: &spec::ChannelSpec) -> Looked {
    let mut seen = upto;
    let fresh = String::from_utf8_lossy(fresh).to_string();
    let Some(marker) = marker else {
        seen += fresh.len() as u64;
        return Looked {
            window: fresh,
            matched: false,
            upto: seen,
            exit: None,
        };
    };
    let Some(at) = fresh.find(marker) else {
        seen += fresh.len() as u64;
        return Looked {
            window: fresh,
            matched: false,
            upto: seen,
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
        upto: upto + (at + marker.len() + eaten) as u64,
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

/// Appends one command to a channel's command file on this machine.
fn append_here(path: &str, line: &str) -> Result<(), String> {
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
