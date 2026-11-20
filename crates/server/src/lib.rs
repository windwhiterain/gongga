//! `gongga-server`: the harness backend, as a library.
//!
//! See `docs/notes/server-lib.md`.

mod agent;
mod agents;
mod anthropic;
pub(crate) use ::relay::base64;
mod blobs;
mod body;
mod bus;
mod clock;
mod commands;
mod config;
mod content_commands;
mod diff;
mod disk;
mod explore;
mod fork;
mod image;
mod instance;
pub mod live;
mod live_compact;
mod llm;
mod meta;
mod migrate;
mod monitor;
mod net;
mod prefix_cache;
mod probe;
mod process;
mod prompts;
mod providers;
mod quota;
pub use ::relay::relay;
pub mod remote;
pub mod remotes;
mod retired_python;
mod script;
mod session;
mod session_config;
mod shell_command;
mod ship;
mod signal;
mod space;
mod store;
mod subscriptions;
pub mod supervise;
mod tool_cmd;
mod tools;
mod transcript;
mod transport;
mod worktree;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use std::io::IsTerminal;
use tokio_util::sync::CancellationToken;

use tracing_subscriber::EnvFilter;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::commands::Dispatch;
use crate::config::Config;
use crate::llm::LlmClient;
use crate::session::SessionManager;
use crate::tools::ToolRegistry;
use crate::tools::detached::DetachedRegistry;

/// What starting a server may be told, and nothing it has to be.
pub struct Args {
    /// Path to config.toml. Defaults to `$GONGGA_HOME/config.toml`.
    pub config: Option<PathBuf>,
    /// UDP address to listen on. Overrides `[server] bind`.
    pub bind: Option<String>,
    /// Model name. Overrides `[provider] model`.
    pub model: Option<String>,
    /// Path to bash. Overrides `GONGGA_BASH` and the detected shell.
    pub bash: Option<PathBuf>,
    /// Home for the database, config, logs, jobs and lock; beats `[server] home`.
    pub home: Option<PathBuf>,
    /// Log filter, e.g. `gongga_server=debug`.
    pub log: String,
    /// How each log line is written: text for a reader, json for a tool.
    pub log_format: LogFormat,
    /// The process that started this server, when its death should stop it.
    pub parent: Option<u32>,
    /// The window's wire protocol; a mismatch cannot read this server.
    pub parent_protocol: Option<u32>,
    /// A handover's old argv: a build hash, not a protocol, so not compared.
    pub parent_build: Option<String>,
    /// Stop whatever holds `--bind` and listen anyway. Without it a taken port
    /// is an error: see `net::bind_address`.
    pub force: bool,
    /// Read the config the way a start would and exit, claiming nothing.
    ///
    /// # Invariant
    ///
    /// The check answers one question -- can this binary read this home's
    /// config -- and stops before the home lock, the database, the port or the
    /// ship install, so running it against a live home touches nothing.
    pub check_config: bool,
}

/// The same as an empty command line: every value comes from the home's config.
impl Default for Args {
    fn default() -> Self {
        Self {
            config: None,
            bind: None,
            model: None,
            bash: None,
            home: None,
            log: "info".to_string(),
            log_format: LogFormat::Text,
            parent: None,
            parent_protocol: None,
            parent_build: None,
            force: false,
            check_config: false,
        }
    }
}

/// The shape of one log line.
///
/// # Invariant
///
/// `json` exists because a benchmark wants a step, a latency, a retry or a
/// turn's outcome, and the text log carries them only as prose with colour
/// escapes. See `docs/notes/server-lib.md`.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

/// The config as this invocation asked for it: the file, then the flags over it.
///
/// # Invariant
///
/// Read once, from the file the content itself keeps: a home has no config of its own
/// any more, so there is no second read to wait for.
fn config_from(args: &Args) -> anyhow::Result<Config> {
    Config::load_with_home(args.config.as_deref(), args.home.as_deref())
}

pub fn main(args: Args) -> anyhow::Result<()> {
    adopt_home(&args);
    // Built here, not by `#[tokio::main]`, so the drop runs after `run` closed
    // every shell. See `docs/notes/server-lib.md`.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(args));
    tracing::info!("main: dropping runtime");
    drop(runtime);
    tracing::info!("main: runtime dropped");
    result
}

/// Makes the home this start resolved the one everything it starts inherits.
///
/// # Invariant
///
/// `--home` beats an inherited `GONGGA_HOME` here, and must beat it for the children
/// too: a relay, a peer's runner and a skill each resolve their own home from
/// `GONGGA_HOME`, so a stale value there puts their files in somebody else's home.
/// Run before the runtime exists, which is the only moment no thread can read it.
fn adopt_home(args: &Args) {
    let Some(named) = args.home.as_ref() else {
        return;
    };
    let home = crate::config::expand(named.clone());
    // SAFETY: this process has one thread until the runtime is built below.
    unsafe {
        std::env::set_var("GONGGA_HOME", &home);
    }
}

/// Where a start's seconds go, one line each.
///
/// # Invariant
///
/// Debug only: a start is read for its steps when a start is questioned, and
/// a step that waits on the world outside the home is a window that cannot open.
struct Clock(std::time::Instant);

impl Clock {
    fn new() -> Self {
        Self(std::time::Instant::now())
    }

    /// Says what the step that just ended cost, and starts the next one.
    fn step(&mut self, step: &str) {
        tracing::debug!(step, ms = self.0.elapsed().as_millis() as u64, "start step");
        self.0 = std::time::Instant::now();
    }
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log));
    match args.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init(),
        // Colour only for a terminal: written to a file, the escapes landed
        // inside the tokens.
        LogFormat::Text => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(std::io::stdout().is_terminal())
            .init(),
    }

    // Flags win; SAFETY: no thread yet, so nothing observes these writes.
    unsafe {
        if let Some(bind) = &args.bind {
            std::env::set_var("GONGGA_BIND", bind);
        }
        if let Some(model) = &args.model {
            std::env::set_var("GONGGA_MODEL", model);
        }
        if let Some(bash) = &args.bash {
            std::env::set_var("GONGGA_BASH", bash);
        }
    }

    // Before anything is spawned: what the supervisor said about this process
    // must not travel into what it starts.
    supervise::remember();
    let mut clock = Clock::new();
    // The content comes first: this home's config is one of its files.
    let home = args.home.clone().unwrap_or_else(crate::config::home_dir);
    let content = if args.check_config {
        ship::inspect(&home)?
    } else {
        ship::prepare(&home)?
    };
    clock.step("ship content");
    let cfg = config_from(&args)?;
    clock.step("config");
    // An unreadable roster refuses the start; see `docs/notes/server-lib.md`.
    if let Some(problem) = providers::roster().problem() {
        anyhow::bail!("the provider roster does not parse: {problem}");
    }
    if args.check_config {
        // A handover's question: answered before the home is claimed, so asking
        // a live home costs it nothing.
        println!("config ok: {}", cfg.config_path.display());
        return Ok(());
    }
    // Claim the home before opening the database: one server per home.
    let _instance = home_lock::HomeLock::acquire(&cfg.home, "server.lock", "gongga server")?;
    instance::record_dir(&cfg.home);
    clock.step("home lock");
    // The one fetch this start makes, from the repository the config names.
    ship::follow(&cfg.home, cfg.ship_url.as_deref());
    clock.step("ship follow");
    let migrated = migrate::bring_forward(&cfg.home, &cfg.db_path)?;
    if !migrated.steps.is_empty() {
        tracing::info!(
            from = migrated.from,
            to = migrate::VERSION,
            steps = ?migrated.steps,
            "home migrated"
        );
    }
    clock.step("migrate");
    // Followed from here: an edit to that file is read at the next request.
    let config = crate::config::Current::load(Some(&cfg.config_path), args.home.as_deref())?;
    let cfg = config.anchor();
    // Before any request is built: templates from this config, over the pin.
    let missing = prompts::install(&cfg.config_path);
    if !missing.is_empty() {
        anyhow::bail!(
            "{} says nothing about {missing:?}, and the pinned ship content (pin {}) \
             cannot answer either; the server would send an empty prompt",
            cfg.config_path.display(),
            content.commit,
        );
    }
    let overridden = prompts::shared().overridden();
    tracing::info!(
        file = %cfg.config_path.display(),
        overridden = ?overridden,
        "prompt templates: the shipped defaults, plus what this home overrode"
    );
    clock.step("prompts");

    let store = store::Store::open(&cfg.db_path)?;
    clock.step("store");
    // Read after every tool call, before a full disk takes the room to report it.
    disk::install(cfg.home.clone());
    if cfg.disk_low == 0 {
        tracing::info!("no disk threshold: this home never reads the room left");
    } else {
        tracing::info!(
            threshold = cfg.disk_low,
            "the room left is read after every tool call, and reported below this"
        );
    }
    // A home with no usable model refuses here, not on a first turn; sessions
    // build their own client.
    drop(LlmClient::new(&cfg.model)?);
    // A script drives this server by the name a shell types: `gongga`. The
    // command has to be there before any shell exists.
    let command = shell_command::ensure_pair(&cfg.home);
    clock.step("shell command");
    if command.is_none() {
        tracing::warn!(home = %cfg.home.display(), "no bin/ in this home: a shell cannot find gongga-cli");
    }
    let mut base_env = vec![
        ("GONGGA_SERVER".to_string(), cfg.bind.to_string()),
        ("GONGGA_HOME".to_string(), cfg.home.display().to_string()),
    ];
    // Named and absolute, so a script does not resolve it through a `PATH` a
    // shell may have rewritten.
    if let Some(command) = &command {
        base_env.push((
            "GONGGA_CLI".to_string(),
            command.to_string_lossy().into_owned(),
        ));
    }
    // A script's interpreter reads a native `PATH`; a shell reads a POSIX one.
    // Both begin with the same directory.
    let mut script_env = base_env.clone();
    script_env.push((
        "PATH".to_string(),
        shell_command::native_path(&cfg.home)
            .to_string_lossy()
            .into_owned(),
    ));
    let mut shell_env = base_env;
    shell_env.push(("PATH".to_string(), shell_command::path(&cfg.home)));
    let detached = DetachedRegistry::new(
        store.clone(),
        crate::config::detect_git_bash(),
        cfg.home.join("detached"),
        shell_env.clone(),
    );
    let sessions = SessionManager::new(
        store.clone(),
        config.clone(),
        detached.clone(),
        shell_env,
        script_env.clone(),
        crate::remotes::Catalog::load(cfg.endpoint.answer_timeout),
    );
    let bus = EventBus::new(4096);

    // The Python surface a session imports, read in the checkout.
    let skill_lib = Some(ship::python(&cfg.home));

    // A provider's own picture is kept beside the home, not in the transcript.
    blobs::install(&cfg.home.join("blobs"));
    // Capped output has to go somewhere the model can read it back.
    let output_dir = cfg.home.join("outputs");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        tracing::warn!(error = %e, "cannot create {output_dir:?}; capped output will be dropped");
    }

    // The commands the content declares, and one table of what each function takes.
    let arguments: crate::tools::Declared = Default::default();
    let declared = content_commands::ContentCommands::new(cfg.home.clone(), arguments.clone());

    // Scripts register commands; the directory is data, so nothing here knows one.
    let scripts = script::Scripts::new(
        cfg.scripts.dir.clone(),
        cfg.scripts.interpreter(),
        script_env,
    );
    if let Err(e) = std::fs::create_dir_all(&cfg.scripts.dir) {
        tracing::warn!(dir = %cfg.scripts.dir.display(), error = %e, "cannot create the scripts directory");
    }
    tracing::info!(scripts = %scripts.describe(), "script commands");
    for problem in scripts.problems().await {
        tracing::warn!(problem = %problem, "a script could not be read");
    }
    clock.step("scripts");

    // The channels `exec` speaks to: a table of programs, not processes.
    let channels = Arc::new(crate::tools::channel::Channels::for_home(
        &cfg,
        store.clone(),
        {
            let store = store.clone();
            let capped_at = cfg.tools.max_output_chars;
            let spill_dir = output_dir.clone();
            Arc::new(move |end: crate::tools::channel::ChannelEnd| {
                // A deleted session owes nothing, and its rows would be stranded.
                if matches!(store.get_session(&end.session), Ok(None)) {
                    return;
                }
                // The notice is the call's own answer, late: the same `Result` a
                // `exec` returns, built by the same one builder.
                let outcome = crate::tools::channel::Outcome {
                    id: end.id.clone(),
                    output: end.output.clone(),
                    time: end.time,
                    matched: end.matched,
                    exit: end.exit,
                };
                let message = outcome.result(capped_at, &spill_dir).to_string();
                // SPEC: `cancel` queues what it leaves; a queued row starts no turn.
                let mut payload = serde_json::json!({
                    "session": end.session.clone(),
                    "message": message,
                    "notice": "channel",
                });
                if !end.waited {
                    payload["queued"] = serde_json::json!(true);
                }
                if let Err(e) = store.add_pending_task(&end.session, "inject", &payload) {
                    tracing::warn!(
                        session = %end.session, channel = %end.id,
                        error = %e, "could not notice a finished channel"
                    );
                }
            })
        },
    ));

    // The directories no row names, swept behind the start: minutes of unlinking.
    {
        let channels = channels.clone();
        tokio::task::spawn_blocking(move || {
            let removed = channels.sweep();
            if removed > 0 {
                tracing::info!(removed, "channel directories no row names were removed");
            }
        });
    }

    let agent = Arc::new(Agent {
        store: store.clone(),
        providers: providers::Handle,
        clients: std::sync::Mutex::new(std::collections::HashMap::new()),
        channels: channels.clone(),
        tools: {
            let lib = skill_lib.clone();
            crate::live::Derived::new(move |cfg: &config::Config| {
                Arc::new(
                    ToolRegistry::settings(cfg.tools.max_output_chars)
                        .with_python(cfg.python.clone(), lib.clone())
                        .with_skills(
                            cfg.python.program.clone(),
                            lib.clone(),
                            std::time::Duration::from_millis(crate::config::CALL_TIMEOUT_MS),
                        )
                        .with_declared(arguments.clone())
                        .build(&cfg.tools.rows),
                )
            })
        },
        bus: bus.clone(),
        config: config.clone(),
        home: cfg.home.clone(),
        output_dir: output_dir.clone(),
    });
    clock.step("tools");

    // Read from the database, so a restart resumes what was asked for; the
    // actions it runs take the agent's tools.
    let subscriptions = crate::subscriptions::Subscriptions::load(store.clone(), bus.clone());
    clock.step("subscriptions");

    let dispatch = Arc::new(Dispatch {
        agent,
        sessions: sessions.clone(),
        store: store.clone(),
        bus: bus.clone(),
        subscriptions: subscriptions.clone(),
        scripts: scripts.clone(),
        declared,
        providers: providers::Handle,
        shutting_down: Arc::new(AtomicBool::new(false)),
        shutdown: CancellationToken::new(),
        // Nothing has been announced yet: the first publish states every row.
        published: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    clock.step("dispatch");

    // Read before any turn, so a forwarding row renders from its function, greeted or not.
    dispatch.catalog().await;
    clock.step("declared");

    // Before any turn: a run that died inside a tool leaves a call no request
    // can carry, and the answer is a row.
    let answered = commands::answer_open_calls(&dispatch);
    if answered > 0 {
        tracing::info!(answered, "transcript repaired before serving");
    }
    // And what a stream was still saying when the process stopped: kept, not lost.
    let settled = commands::settle_drafts(&dispatch);
    if settled > 0 {
        tracing::info!(settled, "half-answers kept before serving");
    }
    clock.step("repair");
    // What a greeting hands every client: the picture a change is diffed against.
    commands::prime_sessions(&dispatch);
    clock.step("prime");
    // SPEC's Dreaming: orphans go before anything resumes; owed dreams are left to it.
    let swept = commands::sweep_orphan_dreams(&dispatch).await;
    if swept > 0 {
        tracing::info!(swept, "orphan dreams discarded before serving");
    }
    // SPEC: this start adopts the channels the last one kept; a remote adoption asks a
    // peer over ssh.
    {
        let channels = Arc::clone(&channels);
        tokio::spawn(async move {
            let adopted_channels = channels.adopt().await;
            if adopted_channels > 0 {
                tracing::info!(
                    adopted_channels,
                    "channels the last run kept, reported gone"
                );
            }
            // And the relays it left running: `cancel(keep_alive)` marked them, so this
            // start takes them over instead of ending what a handover kept.
            let kept = channels.adopt_kept().await;
            if kept > 0 {
                tracing::info!(kept, "channels the last run kept were taken over");
            }
        });
    }
    // Whatever was in flight when the server last stopped is owed work.
    let resumed = commands::resume_pending(&dispatch).await;
    clock.step("resume");
    if resumed > 0 {
        tracing::info!(resumed, "resuming turns left pending by the last run");
    }
    // A heartbeat is armed only at a turn's end: that is when the cache was
    // last warmed. A start is no turn, so no idle session is armed here -- an
    // arm at boot would fire every session in one burst and pretend a restart
    // warmed caches it never touched. See `docs/notes/server-dream.md`.
    // Failures the last run left behind, before a client can ask about them.
    monitor::sweep(&dispatch, &cfg.home).await;
    clock.step("sweep");
    // Whatever the last run detached is still out there; adopt its pids now.
    let adoption_started = std::time::Instant::now();
    let adopted = detached.adopt().await;
    clock.step("detached");
    // Adoption finishes before the port binds, so the duration is always logged.
    tracing::info!(
        adopted,
        elapsed_ms = adoption_started.elapsed().as_millis() as u64,
        "detached jobs adopted from the previous run"
    );

    // The command layer a tool reaches. Weak: the dispatch owns the agent.
    struct Layer(std::sync::Weak<Dispatch>);
    impl crate::commands::CmdLayer for Layer {
        fn run<'a>(
            &'a self,
            session: &'a str,
            name: &'a str,
            args: protocol::Args,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::commands::Outcome> + Send + 'a>,
        > {
            Box::pin(async move {
                let Some(dispatch) = self.0.upgrade() else {
                    return crate::commands::Outcome::err("the server is stopping");
                };
                // The caller rides in the origin, never in the arguments.
                let origin = crate::commands::Origin::Tool {
                    session: session.to_string(),
                };
                crate::commands::dispatch(&dispatch, name, &args, &origin).await
            })
        }
    }
    if !crate::commands::install_layer(std::sync::Arc::new(Layer(Arc::downgrade(&dispatch)))) {
        tracing::warn!("a command layer was already installed; a tool runs the first one");
    }

    let detached_watch = detached.watch(dispatch.shutdown.clone());
    // A channel's end is news while this server is up; the token ends the watching.
    let _channels_watch = channels.watch(dispatch.shutdown.clone());

    // Alive until the drain ends. See `docs/notes/server-subscriptions.md`.
    let watching = subscriptions.clone();
    let stop_watching = CancellationToken::new();
    let matcher = tokio::spawn({
        let stop_watching = stop_watching.clone();
        async move { watching.watch(stop_watching).await }
    });

    // Undelivered messages get a turn, unseen failures get handed over, and an
    // edited roster is republished.
    let wake = dispatch.clone();
    let watch_home = cfg.home.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        // The roster a client was last told about: an edit is news exactly once.
        let mut published = providers::roster().generation();
        loop {
            tick.tick().await;
            let started = commands::nudge_inboxes(&wake);
            if started > 0 {
                tracing::info!(started, "woke sessions holding an undelivered message");
            }
            monitor::sweep(&wake, &watch_home).await;
            // Table and catalog both follow: an argument publishes roster values.
            let roster = providers::roster();
            if roster.generation() != published {
                published = roster.generation();
                tracing::info!(generation = published, "provider roster republished");
                wake.bus.publish(protocol::Event::Models {
                    models: providers::model_table(),
                });
                wake.bus.publish(protocol::Event::Commands {
                    commands: wake.catalog().await,
                });
            }
        }
    });

    tracing::info!(tools = ?dispatch.agent.tools().names().join(", "), "tool registry ready");
    // Which roster was read, and whether it was readable at all: "no --model
    // worked" is nearly always this line.
    tracing::info!(
        rosters = ?providers::roster().paths(),
        providers = providers::roster().providers().len(),
        problem = providers::roster().problem(),
        "provider roster imported"
    );
    tracing::info!(
        key = %cfg.home.join("server.key").display(),
        "clients must present the key from this file (or pass --key)"
    );
    tracing::info!(
        home = %cfg.home.display(),
        // Who restarts this process when it dies. See `server-supervise.md`.
        supervisor = crate::supervise::supervisor(),
        db = %cfg.db_path.display(),
        provider = ?cfg.model.provider,
        model = %cfg.model.model_name,
        base_url = %cfg.model.base_url,
        context = ?cfg.model.context,
        bash = ?crate::config::detect_git_bash(),
        proxy = ?std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("http_proxy"))
            .ok(),
        "gongga server starting"
    );

    // Before the shutdown path: a server that never listened must not wait on
    // tasks it never started.
    let socket = std::sync::Arc::new(net::bind_address(cfg.bind, args.force).await?);

    // Behind the port: the community clone, the fetch's answer, provider scripts.
    // None of them is on a budget.
    agents::start();
    {
        let home = cfg.home.clone();
        tokio::task::spawn_blocking(move || match ship::refresh(&home) {
            Ok(Some(commit)) => tracing::info!(
                content = %commit.get(..10).unwrap_or(commit.as_str()),
                "the content moved while this server ran; the checkout is on it, and this home reads it"
            ),
            Ok(None) => tracing::debug!("content: this home holds what the repository has"),
            Err(e) => tracing::warn!(error = %e, "cannot refresh the shipped content"),
        });
    }

    // A signal, or a dead window: a stop request, on the path `cancel` takes.
    let signalled = dispatch.clone();
    signal::watch(args.parent, args.parent_protocol, move || async move {
        commands::stop(&signalled).await
    });

    let served = net::serve(cfg, socket, dispatch, bus).await;
    // Nothing new may fire once the socket stopped; turns were interrupted already.
    stop_watching.cancel();
    let _ = matcher.await;
    // Nothing notices a job's end after this: a stopping server did not see it finish.
    let _ = detached_watch.await;
    // Every exit from `serve` lands here, so nothing it started outlives the
    // stop decision. See `docs/notes/server-main.md`.
    sessions.closing();
    let stopped = scripts.stop_all().await;
    if stopped > 0 {
        tracing::info!(stopped, "stopped scripts still running at the stop");
    }
    served
}
