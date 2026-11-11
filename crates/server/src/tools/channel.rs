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

use crate::remote::transport::Runner;
use crate::store::{ChannelRow, Store};
use crate::tools::spec;

/// What one call of `exec` answers: [`SPEC`](SPEC.md), "Tool".
///
/// # Invariant
///
/// `output` is the window nobody has read: taking it advances the cursor.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    pub output: String,
    /// Time since the channel's last input, or its start: SPEC's `time`.
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

    /// A call's answer, capped like every tool's output: the same one cap, in the same
    /// unit, applied where the notice is built because the call has already ended.
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
    /// Whether a call had given up waiting: only then does this end owe a turn.
    pub waited: bool,
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
    /// The peers a call may name: this machine's remotes file, followed live.
    remotes: Arc<crate::remotes::Catalog>,
    /// One connection per peer, kept because a channel is read on every look.
    ///
    /// # Invariant
    ///
    /// The server's own, not a session's: a channel outlives the session's shells,
    /// and the relay it reads runs on the peer whether this process is here or not.
    runners: tokio::sync::Mutex<HashMap<String, Arc<Runner>>>,
    /// One dial in flight for one peer, and the outcome the other starts read.
    ///
    /// # Invariant
    ///
    /// No lock is held across `pick::runner`. A start that finds a dial already running
    /// waits for *that* dial's end instead of taking a turn at a mutex, so one peer's
    /// silence cannot queue every later start behind it for ever; and a dial that ended
    /// leaves the map, so the next start dials for itself. See
    /// `docs/notes/server-channel.md`.
    dialing: std::sync::Mutex<HashMap<String, Arc<Dial>>>,
    /// The starts in flight, by row id: what a stop ends, and what one name may not
    /// have twice.
    bringing_up: std::sync::Mutex<HashMap<i64, CancellationToken>>,
    /// The channels a call is inside right now.
    ///
    /// # Invariant
    ///
    /// A call answers with what it saw; the watcher answers what a session was waiting
    /// for. Both would be one end told twice, so the watcher skips what a call holds.
    /// In memory, because a call is only ever in this process.
    attached: std::sync::Mutex<HashSet<i64>>,
}

/// The program a channel's relay is started as.
///
/// # Invariant
///
/// One resolver, not three. `shell_command` names both the source `ensure_pair`
/// installs from and the installed copy a channel runs, so the two cannot be two
/// opinions. A build tree is rebuilt under a running server: the channel holds the
/// copy, not the source a later build is free to replace.
fn relay_program(home: &Path) -> PathBuf {
    crate::shell_command::relay_for_channel(home)
}

impl Channels {
    pub fn new(
        store: Store,
        root: PathBuf,
        server: PathBuf,
        grammar: HashMap<String, spec::ChannelSpec>,
        notify: Notifier,
        remotes: Arc<crate::remotes::Catalog>,
    ) -> Self {
        Self {
            store,
            root,
            server,
            grammar,
            notify,
            remotes,
            runners: tokio::sync::Mutex::new(HashMap::new()),
            dialing: std::sync::Mutex::new(HashMap::new()),
            bringing_up: std::sync::Mutex::new(HashMap::new()),
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
        let server = relay_program(&cfg.home);
        Self::new(
            store,
            cfg.home.join("channels"),
            server,
            cfg.tools.channels.clone().into_iter().collect(),
            notify,
            crate::remotes::Catalog::load(cfg.endpoint.answer_timeout),
        )
    }

    /// A `Channels` nobody speaks to: for a context that reads one answer, writes one
    /// row.
    ///
    /// # Invariant
    ///
    /// The same peers as the server's own: a subscription's action calls `exec` like
    /// any other caller, and a name it cannot resolve would be a name nobody can.
    pub fn without_words(cfg: &crate::config::Config, store: Store, root: PathBuf) -> Self {
        Self::new(
            store,
            root,
            PathBuf::from("gongga-server"),
            HashMap::new(),
            Arc::new(|_| {}),
            crate::remotes::Catalog::load(cfg.endpoint.answer_timeout),
        )
    }

    /// What the config says a program is; a program it does not mention is raw.
    ///
    /// # Invariant
    ///
    /// `[channel.<name>]` is keyed by a program's name, so a path to one is read by
    /// its stem: `C:\Program Files\Git\bin\bash.exe` is `[channel.bash]`.
    fn grammar_of(&self, bin: &str) -> spec::ChannelSpec {
        self.grammar
            .get(bin)
            .or_else(|| self.grammar.get(stem(bin)))
            .cloned()
            .unwrap_or_default()
    }

    /// Starts a program as a channel, or says why it could not.
    ///
    /// # Invariant
    ///
    /// The name is a key of this server's namespace and the directory is where the
    /// bytes live: nothing renames a directory, and nothing reads a channel by a
    /// path on disk. See [`SPEC`](SPEC.md), "Tool".
    pub async fn start(
        self: &Arc<Self>,
        session: &str,
        bin: &str,
        args: &[String],
        remote: Option<&str>,
        cwd: &Path,
        env: &[(String, String)],
        id: &str,
    ) -> Result<ChannelRow, String> {
        let grammar = self.grammar_of(bin);
        let path = id.to_string();
        if self
            .store
            .channel_of(session, &path)
            .map_err(quiet)?
            .is_some()
        {
            return Err(format!(
                "channel {path:?} is in use; `exec` speaks to it, and `del_channel \
                 {path}` ends it"
            ));
        }
        let words = crate::relay::Words {
            program: bin.to_string(),
            build: protocol::version_line().to_string(),
            args: if args.is_empty() {
                grammar.spawn.clone()
            } else {
                args.to_vec()
            },
            // A channel here stands in a directory this machine has; a peer's is its own.
            cwd: match remote {
                Some(_) => cwd.to_path_buf(),
                None => crate::tools::standing_dir(cwd),
            },
            env: env.to_vec(),
        };
        match remote {
            // A program there gets that machine's environment, not this one's.
            Some(machine) => {
                let there = crate::relay::Words {
                    env: Vec::new(),
                    ..words
                };
                self.begin(session, &path, bin, machine, there).await
            }
            None => {
                let (place, pid, relay) = self.start_here(&words).await?;
                self.store
                    .add_channel(
                        session,
                        &path,
                        bin,
                        None,
                        &place.dir(),
                        pid,
                        relay.map(|relay| (relay.pid, relay.started_at)),
                    )
                    .map_err(quiet)?;
                let row = self
                    .store
                    .channel_of(session, &path)
                    .map_err(quiet)?
                    .ok_or_else(|| {
                        format!("channel {path:?} was written and cannot be read back")
                    })?;
                tracing::info!(channel = %path, dir = %place.dir(), pid, "channel started");
                Ok(row)
            }
        }
    }

    /// Registers a channel on a peer, and starts it behind the call.
    ///
    /// # Invariant
    ///
    /// Nothing on another machine is awaited here: the row is written, the start is
    /// spawned, and the caller already has the name. What the peer says -- the directory
    /// it chose, the pid it runs, or why it refused -- reaches this machine through the
    /// row, which is what a [`Self::talk`] waits on. See
    /// `docs/notes/server-channel.md`.
    async fn begin(
        self: &Arc<Self>,
        session: &str,
        path: &str,
        bin: &str,
        machine: &str,
        words: crate::relay::Words,
    ) -> Result<ChannelRow, String> {
        let id = self
            .store
            .begin_channel(session, path, bin, Some(machine))
            .map_err(quiet)?;
        let cancel = CancellationToken::new();
        self.bringing_up
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, cancel.clone());
        let this = Arc::clone(self);
        let (there, spoke, named) = (machine.to_string(), session.to_string(), path.to_string());
        tokio::spawn(async move {
            this.bring_up(id, &spoke, &named, &there, words, cancel)
                .await;
        });
        self.store
            .channel_of(session, path)
            .map_err(quiet)?
            .ok_or_else(|| format!("channel {path:?} was written and cannot be read back"))
    }

    /// Starts one channel on a peer, and records what it said.
    ///
    /// # Invariant
    ///
    /// A failure here is the network's, recorded as such: `start_error`, never an exit
    /// status. A program that never started has no status to give, and a caller told one
    /// would read the peer's silence as the program's own failure.
    ///
    /// # Invariant
    ///
    /// A row that is gone by the time the peer answers is a channel this machine no
    /// longer names, so what the peer started is killed rather than left running for
    /// nobody: the answer to "is this row still mine" is the update's own count.
    async fn bring_up(
        self: Arc<Self>,
        id: i64,
        session: &str,
        path: &str,
        machine: &str,
        words: crate::relay::Words,
        cancel: CancellationToken,
    ) {
        let _registered = BringingUp::of(&self.bringing_up, id);
        if cancel.is_cancelled() {
            let _ = self.store.set_channel_start_error(
                id,
                &format!(
                    "remote {machine}: the start was cancelled before anything there was asked"
                ),
            );
            return;
        }
        match self.start_there(machine, &words).await {
            Ok((place, pid, relay)) => {
                let named = self.store.record_channel_started(
                    id,
                    &place.dir(),
                    pid,
                    relay.map(|relay| (relay.pid, relay.started_at)),
                );
                if named == 0 {
                    // The name went while this start was in flight: nobody can read what
                    // the peer is running, so it is told to end.
                    let _ = place.append("in", "kill\n").await;
                    tracing::info!(channel = %path, "a channel started after its row went; it was told to end");
                    return;
                }
                tracing::info!(channel = %path, session = %session, machine = %machine, dir = %place.dir(), pid, "channel started");
            }
            Err(why) => {
                // The machine is named once: the cause says what, this says where.
                let why = format!("remote {machine}: {why}");
                tracing::warn!(channel = %path, session = %session, why = %why, "a channel could not be started");
                let _ = self.store.set_channel_start_error(id, &why);
            }
        }
    }

    /// A channel on this machine: its own directory, and a relay started beside us.
    ///
    /// # Invariant
    ///
    /// A start that fails leaves nothing behind: no row, no `status`, and no directory
    /// either. The directory is this call's own invention, and a channel nobody holds is
    /// litter no reader can use.
    async fn start_here(
        &self,
        words: &crate::relay::Words,
    ) -> Result<(Where, u32, Option<RelayId>), String> {
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
        let place = Where::Here(dir);
        match self.open_channel(&place, &log, words).await {
            // The relay's own id, which is not the program's: a successor that only
            // has the program's pid can see it is gone, and cannot find the relay.
            Ok((pid, relay)) => Ok((place, pid, Some(relay))),
            Err(why) => {
                if let Where::Here(dir) = &place {
                    let _ = std::fs::remove_dir_all(dir);
                }
                Err(why)
            }
        }
    }

    /// One channel's relay, and the pid it named: this build's copy, then a second look.
    ///
    /// # Invariant
    ///
    /// A relay that will not hold a channel of this protocol is answered with one attempt
    /// to install this build's copy, and no more. A start that cannot be paired is a
    /// failure the caller reads now, with the relay's own words, not a silence that ends
    /// in a deadline.
    async fn open_channel(
        &self,
        place: &Where,
        log: &Path,
        words: &crate::relay::Words,
    ) -> Result<(u32, RelayId), String> {
        match self.launch_relay(&self.server, place, log, words).await {
            Ok(pid) => Ok(pid),
            // A pair that cannot work is the one thing an install answers; anything else
            // is already the answer.
            Err(refused) if refused.may_be_out_of_date() => {
                match self.home().and_then(crate::shell_command::refresh_relay) {
                    Some(fresh) => self
                        .launch_relay(&fresh, place, log, words)
                        .await
                        .map_err(|refused| refused.words),
                    // Nothing to update from: the words name the two protocols, and
                    // that is what this start can say.
                    None => Err(refused.words),
                }
            }
            Err(refused) => Err(refused.words),
        }
    }

    /// One relay, started for one channel directory, and the pid it named.
    ///
    /// # Invariant
    ///
    /// The protocol travels, not the commit: a relay that reads `cmd` and `status` the
    /// way this build writes them holds the channel whatever it was built from. The
    /// build rides along for the refusal's own words alone.
    async fn launch_relay(
        &self,
        relay: &Path,
        place: &Where,
        log: &Path,
        words: &crate::relay::Words,
    ) -> Result<(u32, RelayId), Refused> {
        let mut job = crate::tools::detached::DetachedJob::launch_program(
            relay,
            &[
                "--relay".to_string(),
                place.dir(),
                "--protocol".to_string(),
                crate::relay::PROTOCOL.to_string(),
                "--build".to_string(),
                protocol::BUILD.to_string(),
            ],
            &self.root,
            log,
            &words.env,
        )
        .map_err(|e| Refused {
            code: None,
            words: format!(
                "could not start a relay for {:?} with {}: {e}",
                words.program,
                relay.display()
            ),
        })?;
        // In this server's tree, so a kill takes the channel with it: only a `cancel`
        // lets them go.
        #[cfg(windows)]
        crate::tools::job::arm(job.pid);
        // Read before the child is handed over: after this the job is the channel's.
        let relay = RelayId {
            pid: job.pid,
            started_at: job.started_at,
        };
        self.await_status(place, &log.display().to_string(), job.take_child())
            .await
            .map(|pid| (pid, relay))
    }

    /// This home, when `root` is the `channels` directory of one.
    fn home(&self) -> Option<&Path> {
        let home = self.root.parent()?;
        (self.root.file_name()? == "channels").then_some(home)
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
    ) -> Result<(Where, u32, Option<RelayId>), String> {
        let runner = self.runner_for(machine).await?;
        // `.` is the call naming no directory: the program then runs where the peer's
        // own runner stands.
        let named = words.cwd != Path::new(".");
        if named {
            let asked = words.cwd.display().to_string();
            let there = runner.stat(&asked).await.map_err(|e| format!("{e}"))?;
            if !there.is_dir {
                return Err(format!("it has no directory {asked:?}"));
            }
        }
        let written = serde_json::to_vec(words)
            .map_err(|e| format!("could not describe {:?}: {e}", words.program))?;
        let cwd = named.then(|| words.cwd.display().to_string());
        let started = runner
            .relay(cwd.as_deref(), &written)
            .await
            .map_err(|e| format!("a channel could not be started there: {e}"))?;
        if started.dir.is_empty() {
            return Err("it started a relay and named no directory".to_string());
        }
        let place = Where::There {
            runner,
            dir: started.dir,
        };
        let pid = self
            .await_status(&place, &started.log, None)
            .await
            .map_err(|refused| refused.words)?;
        // A peer started the relay, so this machine holds no handle for it and cannot
        // adopt it: the flag on the row is still set for it, but adoption here is a
        // no-op. Remote channels are their own problem -- see the note on the row.
        Ok((place, pid, None))
    }

    /// The connection a peer answers on, started once and kept.
    ///
    /// # Invariant
    ///
    /// One per peer name, and a connection that has ended is not reused: a peer that
    /// was restarted must not answer with the silence of the run before it.
    async fn runner_for(&self, machine: &str) -> Result<Arc<Runner>, String> {
        if let Some(runner) = self.connected_runner(machine).await {
            return Ok(runner);
        }
        let Some(endpoint) = self.remotes.find(machine).map(|remote| remote.endpoint) else {
            let mut known = self.remotes.names();
            known.sort_unstable();
            return Err(format!(
                "no remote named {machine:?}; this machine's remotes are {}; `none` (or `local`) is this machine",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        };
        // One dial per peer, shared: a start joins the one running instead of queueing.
        let (dial, mine) = self.dial_entry(machine);
        if !mine {
            let said = self.join(&dial).await;
            if let Some(runner) = self.connected_runner(machine).await {
                return Ok(runner);
            }
            return Err(match said {
                Some(why) if !why.is_empty() => why,
                _ => format!("{machine}: no connection could be made to it"),
            });
        }
        let spawned = crate::remote::pick::runner(&endpoint, endpoint.answer_timeout()).await;
        match spawned {
            Ok(runner) => {
                let runner = Arc::new(runner);
                // Kept before the joiners are told, so one that wakes finds it here.
                self.runners
                    .lock()
                    .await
                    .insert(machine.to_string(), Arc::clone(&runner));
                let _ = dial.ended.send(Some(String::new()));
                self.dial_over(machine, &dial);
                Ok(runner)
            }
            Err(e) => {
                let why = format!("{e}");
                let _ = dial.ended.send(Some(why.clone()));
                self.dial_over(machine, &dial);
                Err(why)
            }
        }
    }

    /// The dial of one peer, and whether this caller is the one that runs it.
    fn dial_entry(&self, machine: &str) -> (Arc<Dial>, bool) {
        let mut dialing = self.dialing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = dialing.get(machine)
            && running.running()
        {
            return (Arc::clone(running), false);
        }
        let (ended, _) = tokio::sync::watch::channel(None);
        let dial = Arc::new(Dial { ended });
        dialing.insert(machine.to_string(), Arc::clone(&dial));
        (dial, true)
    }

    /// Waits for the dial this start joined, and answers how it ended.
    ///
    /// # Invariant
    ///
    /// The wait is the dial's, not a lock's: it ends when that dial does, which its own
    /// steps bound. `None` is a dial whose owner vanished without a word -- this caller
    /// then dials for itself rather than waiting for a start that will never come.
    async fn join(&self, dial: &Arc<Dial>) -> Option<String> {
        let mut ended = dial.ended.subscribe();
        loop {
            if let Some(said) = ended.borrow().clone() {
                return Some(said);
            }
            if ended.changed().await.is_err() {
                return None;
            }
        }
    }

    /// Takes this dial's entry out, without disturbing one a later start replaced.
    fn dial_over(&self, machine: &str, dial: &Arc<Dial>) {
        let mut dialing = self.dialing.lock().unwrap_or_else(|e| e.into_inner());
        if dialing
            .get(machine)
            .is_some_and(|held| Arc::ptr_eq(held, dial))
        {
            dialing.remove(machine);
        }
    }

    /// One peer's kept connection, while it is still the one that answers.
    ///
    /// # Invariant
    ///
    /// The table is locked for the look alone: no caller may wait on a peer while
    /// holding what every other caller needs.
    async fn connected_runner(&self, machine: &str) -> Option<Arc<Runner>> {
        let runners = self.runners.lock().await;
        runners
            .get(machine)
            .filter(|runner| runner.connected())
            .map(Arc::clone)
    }

    /// Where one channel's files are, for a reader that must not wait on a peer.
    ///
    /// # Invariant
    ///
    /// only a wait dials. Everything else -- `ls_channel`, `reap`, `del_channel`, a stop,
    /// the watcher -- answers from what this machine already has: a row, and a connection
    /// that is up. [`Self::place_waiting`] is the one that may dial, and `talk` calls it
    /// with the call's own clock.
    async fn place(&self, row: &ChannelRow) -> Result<Where, String> {
        match &row.remote {
            None => Ok(Where::Here(PathBuf::from(&row.dir))),
            Some(machine) => {
                if row.dir.is_empty() {
                    return Err(self.starting(row, machine));
                }
                match self.connected_runner(machine).await {
                    Some(runner) => Ok(Where::There {
                        runner,
                        dir: row.dir.clone(),
                    }),
                    None => Err(format!(
                        "remote {machine}: its connection is not up, so this channel \
                         cannot be read from here"
                    )),
                }
            }
        }
    }

    /// The same place, dialled when the peer's connection is not up: only a wait asks.
    async fn place_waiting(&self, row: &ChannelRow) -> Result<Where, String> {
        let Some(machine) = row.remote.clone() else {
            return self.place(row).await;
        };
        if row.dir.is_empty() {
            return Err(self.starting(row, &machine));
        }
        if let Some(runner) = self.connected_runner(&machine).await {
            return Ok(Where::There {
                runner,
                dir: row.dir.clone(),
            });
        }
        Ok(Where::There {
            runner: self.runner_for(&machine).await?,
            dir: row.dir.clone(),
        })
    }

    /// Tells the start of one row to stop, when it has not got as far as a channel.
    ///
    /// # Invariant
    ///
    /// A stop is an answer, not a wait: the row is the name's, and a start that is still
    /// in flight ends itself when it sees the name gone. See [`Channels::bring_up`].
    fn cancel_start(&self, id: i64) {
        let cancel = self
            .bringing_up
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned();
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
    }

    /// What a row that has no directory yet says, in the words of the network.
    fn starting(&self, row: &ChannelRow, machine: &str) -> String {
        match &row.start_error {
            Some(why) => why.clone(),
            None => format!("remote {machine}: this channel is still starting there"),
        }
    }

    /// One channel of one session, when this session has one by that name.
    pub fn find(&self, session: &str, id: &str) -> Result<Option<ChannelRow>, String> {
        self.store.channel_of(session, id).map_err(quiet)
    }

    /// The pid the relay named, once it has started the program.
    ///
    /// # Invariant
    ///
    /// A relay that is gone without naming one has answered: its refusal is all a start
    /// can say, and waiting out `START_DEADLINE` cannot change it. One that is still
    /// there may still be reading its words, so the deadline stays for it. `child` is
    /// `None` for a peer's relay, whose end this side sees through the connection.
    async fn await_status(
        &self,
        place: &Where,
        log: &str,
        mut child: Option<tokio::process::Child>,
    ) -> Result<u32, Refused> {
        let deadline = Instant::now() + START_DEADLINE;
        loop {
            if let Some(status) = place.status().await {
                return Ok(status.pid);
            }
            if let Some(code) = ended(&mut child) {
                // A pid written between the look above and this one is a start, not a
                // refusal.
                if let Some(status) = place.status().await {
                    return Ok(status.pid);
                }
                return Err(Refused {
                    code,
                    words: words_in(log),
                });
            }
            if Instant::now() >= deadline {
                return Err(Refused {
                    code: None,
                    words: format!(
                        "a relay started but never said who it started; its own words are in {log}"
                    ),
                });
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
        cancel: Option<&CancellationToken>,
        inbox: Option<&crate::session::InboxSignal>,
    ) -> Result<Outcome, String> {
        let mut row = self.row_of(session, id)?;
        // Whoever is inside the channel is the one that tells the session: the watcher
        // skips what a call holds.
        let _held = self.attach(row.id);
        // SPEC: every `exec` resets the flag; only a call that ran out leaves it set.
        self.store.take_channel_wait(row.id).map_err(quiet)?;
        // `time` is what this call cost: from its own write, or from the last one.
        let mut since = row.wrote_at.unwrap_or(row.started_at);
        // One clock for the whole call: a start, a read and a status are all inside it.
        let deadline = Instant::now() + wait;
        // A peer's channel is a name before it is a program: this waits on its start.
        while row.dir.is_empty() {
            if let Some(why) = row.start_error.clone() {
                // The network's own words, never an exit status, and the name goes with
                // the answer so the next call can use it again.
                let _ = self.store.claim_channel_end(row.id);
                return Err(why);
            }
            if Instant::now() >= deadline || cancel.is_some_and(CancellationToken::is_cancelled) {
                if !wait.is_zero() {
                    self.store
                        .set_channel_wait(session, id, "")
                        .map_err(quiet)?;
                }
                return Ok(Outcome {
                    id: row.path.clone(),
                    output: format!(
                        "[remote {}: this channel is still starting there; nothing has been \
                         said yet]",
                        row.remote.clone().unwrap_or_default()
                    ),
                    time: Duration::from_millis(
                        (now_millis() - since).max(0).try_into().unwrap_or(0),
                    ),
                    matched: false,
                    exit: None,
                });
            }
            tokio::time::sleep(LOOK).await;
            row = self.row_of(session, id)?;
        }
        // Only a wait may dial, and it does so inside the clock above.
        let place = self.place_waiting(&row).await?;
        let grammar = self.grammar_of(&row.program);
        // SPEC's util is sticky: the marker the last write left, so a read knows its end.
        let sentinel = if grammar.until.as_deref().is_some_and(has_sentinel) {
            match (row.until_sentinel.is_empty(), input.is_some()) {
                (true, true) => {
                    let fresh = new_sentinel();
                    self.store
                        .set_channel_sentinel(session, id, &fresh)
                        .map_err(quiet)?;
                    Some(fresh)
                }
                (false, _) => Some(row.until_sentinel.clone()),
                // Nothing has been written yet, so no reply is waiting to end.
                (true, false) => None,
            }
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
        let upto = row.upto;
        loop {
            // # Invariant
            //
            // The peer reads race the stop, the clock and a message: a peer that never
            // answers must not hold the call past its wait, an interrupt, or a message.
            // The check cannot sit after the read -- a wedged read never reaches it.
            let read = async {
                let fresh = if wait.is_zero() {
                    place.read_from("out", upto).await?
                } else {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match tokio::time::timeout(left, place.read_from("out", upto)).await {
                        Ok(read) => read?,
                        Err(_) => Vec::new(),
                    }
                };
                let looked = look(&fresh, upto, marker.as_deref(), &grammar);
                let ended = if wait.is_zero() {
                    place.status().await.and_then(|status| status.exit)
                } else {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match tokio::time::timeout(left, place.status()).await {
                        Ok(status) => status.and_then(|status| status.exit),
                        Err(_) => None,
                    }
                };
                Ok::<_, String>((looked, ended))
            };
            tokio::pin!(read);
            let (looked, ended) = if wait.is_zero() {
                read.await?
            } else {
                tokio::select! {
                    read = &mut read => read?,
                    _ = cancel_fired(cancel) => {
                        return self.answer(row, String::new(), false, upto, None, None, since);
                    }
                    _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) => {
                        return self.answer(row, String::new(), false, upto, None, None, since);
                    }
                    _ = inbox_arrived(inbox) => {
                        return self.answer(row, String::new(), false, upto, None, None, since);
                    }
                }
            };
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
        if gone.is_some() {
            // The program is the channel: its end removes the row, and frees the name.
            self.store.claim_channel_end(row.id).map_err(quiet)?;
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
            let unread = match self.place(row).await {
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
                "unread": unread,
            }));
        }
        Ok(listed)
    }

    /// Ends a channel, and forgets it.
    ///
    /// # Invariant
    ///
    /// `kill` in the command file, then the row, then this home's directory: a channel
    /// whose program is still writing must not look gone while it is not, and the files
    /// outlive the row only as litter nobody can name.
    ///
    /// # Invariant
    ///
    /// `Ok` means the row is gone, and that is the whole answer: a channel whose program
    /// had already exited is still a row this forgets, so there is no "was running" to
    /// report -- the name a caller reads afterwards is gone either way.
    pub async fn stop(&self, session: &str, id: &str) -> Result<(), String> {
        let row = self.row_of(session, id)?;
        if row.dir.is_empty() {
            // Nothing on the peer is named yet: the start stops, and the name goes.
            self.cancel_start(row.id);
            self.store.delete_channel(session, id).map_err(quiet)?;
            return Ok(());
        }
        let place = self.place(&row).await?;
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
        // The files were the row's; a peer's directory is not this machine's to remove.
        if let Where::Here(dir) = &place {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    /// Removes the directories no row names: what an ended channel used to leave behind.
    ///
    /// # Invariant
    ///
    /// A directory younger than [`GRACE`] belongs to a start that may still be writing its
    /// row, and is left alone; a name the table holds is a channel somebody can still
    /// read. That is what lets this run behind a serving server: unlinking thousands of
    /// them is minutes of work, and no reader waits for it.
    pub fn sweep(&self) -> usize {
        let rows = match self.store.every_channel() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("could not read the channels to sweep: {e}");
                return 0;
            }
        };
        // A peer's directory is not here; only this machine's rows can vouch for a name.
        let known: std::collections::HashSet<String> = rows
            .iter()
            .filter(|row| row.remote.is_none())
            .map(|row| named(&row.dir))
            .collect();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return 0;
        };
        let mut gone = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("ch-") || known.contains(&name) {
                continue;
            }
            let fresh = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|when| when.elapsed().is_ok_and(|age| age < GRACE));
            if fresh {
                continue;
            }
            if std::fs::remove_dir_all(entry.path()).is_ok() {
                gone += 1;
            }
        }
        gone
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

    /// Marks every channel as one a `cancel(keep_alive)` asked to keep.
    ///
    /// # Invariant
    ///
    /// The flag is a state on the row, so it outlives the stop that set it: a window
    /// leaving later, or a second handover, spares these too. That is the whole
    /// difference between this and passing `keep_alive` once.
    pub async fn mark_kept(&self) -> usize {
        match self.store.mark_detached() {
            Ok(marked) => marked,
            Err(e) => {
                tracing::warn!("could not mark the channels to keep: {e}");
                0
            }
        }
    }

    /// Ends every channel no `cancel` was asked to keep; one this server cannot reach
    /// is left as the row it is.
    ///
    /// # Invariant
    ///
    /// `kill` in the command file and the row gone: the pair `stop_session` writes one
    /// session at a time. A row left behind would be adopted by the next start as a
    /// channel that answers nothing.
    ///
    /// A row carrying `detached` is spared: a `cancel(keep_alive)` asked for it, and
    /// SPEC's "make every channel detached" is a state, not one stop's argument. So a
    /// window leaving later does not end what an earlier handover kept.
    pub async fn stop_every(&self) -> usize {
        let rows = match self.store.every_channel() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("could not read the live channels: {e}");
                return 0;
            }
        };
        let mut ended = 0;
        for row in rows {
            if row.detached {
                continue;
            }
            if row.dir.is_empty() {
                self.cancel_start(row.id);
                self.notice_end(
                    &row,
                    "[the server stopped this channel before it started]".to_string(),
                    row.upto,
                    None,
                    false,
                    true,
                );
                continue;
            }
            match self.place(&row).await {
                Ok(place) => {
                    // What it printed until the kill: the notice is a `Result`, and
                    // an empty one would say nothing about why it stopped.
                    let printed = place.read_from("out", row.upto).await.unwrap_or_default();
                    if place.append("in", "kill\n").await.is_ok() {
                        ended += 1;
                    }
                    // SPEC: killing a channel produces its end notice; the row is gone.
                    self.notice_end(
                        &row,
                        format!(
                            "{}\n\n[the server stopped this channel as it was cancelled; \
                             this is what it had printed until then]",
                            String::from_utf8_lossy(&printed)
                        ),
                        row.upto,
                        None,
                        false,
                        true,
                    );
                }
                Err(e) => tracing::warn!(channel = %row.path, "{e}"),
            }
        }
        ended
    }

    /// Says which channels the last run kept are still there, and which are not.
    ///
    /// # Invariant
    ///
    /// SPEC: "failed to adopting channel will produce channel end notice". Adoption
    /// is not a process either -- a kept channel is read and written through its
    /// files -- so the failure is found the same way the success is: its status says
    /// it ended, and the server that promised to adopt it is the one that has to say
    /// so. Finished first, then told: a row that is gone cannot be noticed twice.
    pub async fn adopt(&self) -> usize {
        let rows = match self.store.every_channel() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("could not read the channels to adopt: {e}");
                return 0;
            }
        };
        let mut gone = 0;
        for row in &rows {
            if row.dir.is_empty() {
                // A start the last run did not finish: nothing on the peer is named.
                if self.store.claim_channel_end(row.id).unwrap_or(false) {
                    gone += 1;
                }
                continue;
            }
            if self.reap(row).await.is_some() {
                gone += 1;
            }
        }
        if gone > 0 {
            tracing::info!(gone, "channels the last run kept were gone when adopted");
        }
        gone
    }

    /// Takes over the relays a `cancel(keep_alive)` left running, and says how many.
    ///
    /// # Invariant
    ///
    /// The flag is what makes this possible: `cancel` marked the row, so a start knows
    /// which channels were kept on purpose rather than left behind. The relay's own id
    /// is on the row, because the program's pid cannot name it.
    ///
    /// A relay whose pid is gone, or whose process no longer matches `relay_started_at`,
    /// ended while no server held it: the row goes and the channel is noticed, as
    /// [`Self::reap`] does. A row with no relay id is left marked -- this machine never
    /// held that handle, so adopting it is nothing this side can do.
    pub async fn adopt_kept(&self) -> usize {
        let rows = match self.store.detached_channels() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("could not read the channels to adopt: {e}");
                return 0;
            }
        };
        let mut adopted = 0;
        for row in &rows {
            let Some(relay) = row.relay_pid else {
                continue;
            };
            let Ok(relay) = u32::try_from(relay) else {
                continue;
            };
            // Before the pid is trusted: an arm kills on close, so a recycled pid would
            // make this server's death somebody else's.
            match crate::tools::detached::adopt_kept_relay(relay, row.relay_started_at) {
                Ok(true) => {}
                Ok(false) => {
                    // The relay is gone: the channel it held ended with it.
                    if self.reap(row).await.is_some() {
                        adopted += 1;
                    }
                    continue;
                }
                Err(e) => {
                    tracing::warn!(channel = %row.path, error = %e, "could not verify a kept relay");
                    continue;
                }
            }
            // The flag is spent: the relay is this server's now, and one more start must
            // not offer it again.
            let _ = self.store.clear_detached(row.id);
            adopted += 1;
        }
        if adopted > 0 {
            tracing::info!(adopted, "channels a previous run kept were taken over");
        }
        adopted
    }

    /// Ends one channel whose program is already gone, and says the status it ended with.
    ///
    /// # Invariant
    ///
    /// Removing the row is the claim, so one end is told once, and the name it held is
    /// free afterwards. `None` is a program that is still there: the name is not the
    /// caller's to take.
    pub async fn reap(&self, row: &ChannelRow) -> Option<i32> {
        // A start that failed is not a program: the row goes, and no exit is answered.
        if row.start_error.is_some() {
            let _ = self.store.claim_channel_end(row.id);
            return None;
        }
        let place = self.place(row).await.ok()?;
        let exit = place.status().await.and_then(|status| status.exit)?;
        let printed = place.read_from("out", row.upto).await.unwrap_or_default();
        self.notice_end(
            row,
            format!(
                "{}\n\n[its program had already ended, with exit {exit}; nothing was left to keep alive]",
                String::from_utf8_lossy(&printed)
            ),
            row.upto,
            Some(exit),
            false,
            true,
        );
        Some(exit)
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
            let alive = if row.dir.is_empty() {
                self.cancel_start(row.id);
                false
            } else {
                match self.place(&row).await {
                    Ok(place) => place.append("in", "kill\n").await.is_ok(),
                    Err(e) => {
                        tracing::warn!(channel = %row.path, "{e}");
                        false
                    }
                }
            };
            if alive {
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

    /// The one door for a channel's end news: the wait flag is the one-speaker token.
    fn notice_end(
        &self,
        row: &ChannelRow,
        output: String,
        read_to: u64,
        exit: Option<i32>,
        matched: bool,
        end_row: bool,
    ) {
        // A call inside it answers its session itself; saying it again is the double.
        if self.is_attached(row.id) {
            return;
        }
        let waited = self.store.take_channel_wait(row.id).unwrap_or(false);
        let claimed = if end_row {
            self.store.claim_channel_end(row.id).unwrap_or(false)
        } else {
            true
        };
        // A notice is a read: it carries the window, so the cursor moves with it.
        let _ = self.store.set_channel_upto(row.id, read_to);
        if waited && claimed {
            // A notice is a call's `wait` run out: say so, and leave SPEC's `Result` alone.
            let output = format!(
                "{output}\n\n[a call's wait ran out before this arrived; this is the rest of that call's answer]"
            );
            (self.notify)(ChannelEnd {
                session: row.session.clone(),
                id: row.path.clone(),
                output,
                // SPEC's `time`: since the last input.
                time: Duration::from_millis(
                    (now_millis() - row.wrote_at.unwrap_or(row.started_at))
                        .max(0)
                        .try_into()
                        .unwrap_or(0),
                ),
                exit,
                matched,
                waited,
            });
        }
    }

    /// Watches every live channel, and tells a session what it is waiting for.
    ///
    /// # Invariant
    ///
    /// A notice is one more `exec`: it reads the same window, so a session is never
    /// told bytes it already read and never loses the tail. The stop token keeps a
    /// `cancel` from announcing what it just ended.
    ///
    /// Only a flagged channel speaks, and once: `exec` sets it, the call clears
    /// it when it answers, and the claim lets the watcher speak for one no call is in.
    pub fn watch(self: &Arc<Self>, stop: CancellationToken) -> tokio::task::JoinHandle<()> {
        let channels = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(LOOK) => {}
                }
                let live = match channels.store.every_channel() {
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
                    if let Some(why) = row.start_error.clone() {
                        // A failed start speaks in the network's words, never as an exit.
                        channels.notice_end(&row, why, row.upto, None, false, true);
                        continue;
                    }
                    let Ok(place) = channels.place(&row).await else {
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
                    channels.notice_end(
                        &row,
                        looked.window,
                        looked.upto,
                        ended,
                        looked.matched,
                        ended.is_some(),
                    );
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

/// A relay that did not hold a channel: how it ended, and what it said.
///
/// # Invariant
///
/// The words are the relay's own, because they name the two programs that disagree. A
/// caller that can do something about the pair -- install the copy beside this server --
/// reads [`Refused::may_be_out_of_date`] first, and reports these words when that does
/// not land, instead of a deadline that says nothing.
struct Refused {
    /// `None` when it never started, or never ended: the two cases a code cannot tell.
    code: Option<i32>,
    words: String,
}

impl Refused {
    /// Whether this is a relay that could be replaced by this build's copy.
    ///
    /// # Invariant
    ///
    /// The three codes a relay answers with when it will not hold what it was handed:
    /// `2` (it does not know these words at all), [`REFUSED`](crate::relay::REFUSED)
    /// (another protocol), and `None` (there was nothing to start). Anything else is a
    /// relay that ran and failed, which no install changes.
    fn may_be_out_of_date(&self) -> bool {
        match self.code {
            Some(code) => code == 2 || code == i32::from(crate::relay::REFUSED),
            None => true,
        }
    }
}

/// The code a child ended with, once it has ended.
fn ended(child: &mut Option<tokio::process::Child>) -> Option<Option<i32>> {
    let status = child.as_mut()?.try_wait().ok()??;
    Some(status.code())
}

/// The last thing a relay's log says, for an error that has to carry it.
///
/// # Invariant
///
/// A tail, not the file: a relay that spent a day holding a channel has said nothing in
/// it, but the one line that matters is written as it starts.
fn words_in(log: &str) -> String {
    let read = std::fs::read(log).unwrap_or_default();
    let tail = read.len().saturating_sub(4096);
    let text = String::from_utf8_lossy(&read[tail..]);
    let text = text.trim();
    if text.is_empty() {
        format!("the relay ended without a word; its log is {log}")
    } else {
        format!("the relay ended: {text}")
    }
}

/// How long a directory is given to be named by a row before a sweep may take it.
///
/// # Invariant
///
/// A start writes its row within milliseconds of making the directory. This is the margin
/// between the two, and it is what lets the sweep run behind a serving server.
const GRACE: Duration = Duration::from_secs(120);

/// The directory name a row's path ends in, in either spelling of the separator.
fn named(dir: &str) -> String {
    dir.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(dir)
        .to_string()
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

/// A start's own entry in the bring-up registry, released when the start answers.
struct BringingUp<'a> {
    held: &'a std::sync::Mutex<HashMap<i64, CancellationToken>>,
    id: i64,
}

impl<'a> BringingUp<'a> {
    fn of(held: &'a std::sync::Mutex<HashMap<i64, CancellationToken>>, id: i64) -> Self {
        Self { held, id }
    }
}

impl Drop for BringingUp<'_> {
    fn drop(&mut self) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// One dial to one peer, and the outcome every other start that joined it reads.
///
/// # Invariant
///
/// `None` is a dial still running, `Some("")` one that connected, `Some(why)` one that
/// failed. Starts read it instead of taking a lock, so a peer that never answers costs
/// its own start and no other. See `docs/notes/server-channel.md`.
struct Dial {
    ended: tokio::sync::watch::Sender<Option<String>>,
}

impl Dial {
    fn running(&self) -> bool {
        self.ended.borrow().is_none()
    }
}

/// A relay's own identity: the pid it runs as, and when it started.
///
/// # Invariant
///
/// Not the same as the channel's `pid`, which is the *program's* one, read from the
/// relay's `status`. A successor that holds only the program's pid can see the
/// program is gone and cannot find the relay to adopt -- so the relay's own id is
/// stored with the row, and a recycled pid is refused by `started_at` before a job
/// is armed for it.
#[derive(Debug, Clone, Copy)]
struct RelayId {
    pid: u32,
    started_at: i64,
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
                    // Every chunk moves the cursor; one that does not is a broken
                    // runner, not a long file.
                    if chunk.next <= offset {
                        return Err(format!(
                            "the endpoint answered {path:?} at offset {offset} without moving \
                             its cursor to {}: this runner cannot be read",
                            chunk.next
                        ));
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

/// A stop that may never come: `None` is a wait no interrupt ends.
async fn cancel_fired(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// A message for the session, polled on the channel's own clock: the signal is a
/// count, so arriving is one load, and the gap between loads is the channel's look.
async fn inbox_arrived(inbox: Option<&crate::session::InboxSignal>) {
    match inbox {
        Some(signal) => loop {
            if signal.arrived() {
                return;
            }
            tokio::time::sleep(LOOK).await;
        },
        None => std::future::pending::<()>().await,
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
/// Longest tail of `fresh` that is a prefix of `marker`, kept so a split marker can still match.
fn marker_prefix_tail(fresh: &str, marker: &str) -> usize {
    let (f, m) = (fresh.as_bytes(), marker.as_bytes());
    let max = f.len().min(m.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|&k| f[f.len() - k..] == m[..k])
        .unwrap_or(0)
}

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
        // Keep a mid-write marker's prefix unconsumed so the next read can match it whole.
        let end = fresh.len() - marker_prefix_tail(&fresh, marker);
        seen += end as u64;
        return Looked {
            window: fresh.get(..end).unwrap_or(fresh.as_str()).to_string(),
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

/// The name a program is known by: its file name, without the `.exe` it is run by.
///
/// # Invariant
///
/// A name with no directory and no suffix is itself, so `[channel.bash]` answers both
/// `bash` and the path Git installs it at.
pub fn stem(bin: &str) -> &str {
    let name = bin.rsplit(['/', '\\']).next().unwrap_or(bin);
    match name.rsplit_once('.') {
        Some((stem, suffix)) if !stem.is_empty() && suffix.eq_ignore_ascii_case("exe") => stem,
        _ => name,
    }
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
