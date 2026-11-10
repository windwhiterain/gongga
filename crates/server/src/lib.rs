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
mod pylib;
mod quota;
pub use ::relay::relay;
pub mod remote;
pub mod remotes;
mod script;
mod session;
mod shell_command;
mod ship;
mod signal;
mod store;
mod subscriptions;
pub mod supervise;
mod tool_cmd;
mod tools;
mod transcript;
mod transport;
pub mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};

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
    /// Path to bash. Overrides `[tools.bash] program`.
    pub bash: Option<PathBuf>,
    /// Home for the database, config, logs, jobs and lock; beats `[server] home`.
    pub home: Option<PathBuf>,
    /// Log filter, e.g. `gongga_server=debug`.
    pub log: String,
    /// How each log line is written: text for a reader, json for a tool.
    pub log_format: LogFormat,
    /// The process that started this server, when its death should stop it.
    pub parent: Option<u32>,
    /// That process's build, when the one that started this named it.
    pub parent_build: Option<String>,
    /// Stop whatever holds `--bind` and listen anyway. Without it a taken port
    /// is an error: see `net::bind_address`.
    pub force: bool,
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
            parent_build: None,
            force: false,
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
/// Read once before the shipped content is installed, for the home alone, and
/// once after, for everything else: a value read from a file that had not
/// arrived yet is the loader's default, not this home's.
fn config_from(args: &Args) -> anyhow::Result<Config> {
    Config::load_with_home(args.config.as_deref(), args.home.as_deref())
}

pub fn main(args: Args) -> anyhow::Result<()> {
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
    let cfg = config_from(&args)?;
    // Claim the home before opening the database: one server per home.
    let _instance = home_lock::HomeLock::acquire(&cfg.home, "server.lock", "gongga server")?;
    instance::record_dir(&cfg.home);
    // Off this thread before anything reads a roster: no start waits for a
    // network. See `docs/notes/server-agents.md`.
    agents::start();
    // Before anything resolves: the lookup is memoized, one content per server.
    ship::remember_url(cfg.ship_url.clone());
    // Before anything reads this home: the content this start installs and the
    // migrator, resolved once so they agree.
    ship::prepare(&cfg.home);
    let migrated = migrate::bring_forward(&cfg.home, &cfg.db_path)?;
    if !migrated.steps.is_empty() {
        tracing::info!(
            from = migrated.from,
            to = migrate::VERSION,
            steps = ?migrated.steps,
            "home migrated"
        );
    }

    // Installed before anything reads it: a config already here is the reader's.
    let installed = ship::install(&cfg.home)?;
    tracing::info!(
        content = %installed.pin.get(..10).unwrap_or(installed.pin.as_str()),
        written = installed.written,
        updated = installed.updated,
        merged = installed.merged,
        removed = installed.removed,
        conflicts = ?installed.conflicts,
        reachable = installed.reachable,
        "shipped content"
    );
    // Read again: the install may have just placed this home's config. From here
    // the file is followed, so an edit reaches the next request, not the next start.
    let config = crate::config::Current::load(args.config.as_deref(), args.home.as_deref())?;
    let cfg = config.anchor();
    // Before any request is built: templates from this config, over the pin.
    let missing = prompts::install(&cfg.config_path);
    if !missing.is_empty() {
        anyhow::bail!(
            "{} says nothing about {missing:?}, and the pinned ship content (pin {}) \
             cannot answer either; the server would send an empty prompt",
            cfg.config_path.display(),
            installed.pin,
        );
    }
    let overridden = prompts::shared().overridden();
    tracing::info!(
        file = %cfg.config_path.display(),
        overridden = ?overridden,
        "prompt templates: the shipped defaults, plus what this home overrode"
    );

    let store = store::Store::open(&cfg.db_path)?;
    store.set_live_compact_enabled(cfg.agent.live_compact_enabled);
    // Read after every tool call, before a full disk takes the room to report it.
    disk::install(cfg.disk_low, cfg.home.clone());
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
        cfg.bash.program.clone(),
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

    // Where a session's Python imports the skill contract and the client.
    let skill_lib = match pylib::install(&cfg.home) {
        Ok(dir) => Some(dir),
        Err(e) => {
            tracing::warn!(error = %e, "cannot install gongga_skill.py; a package skill's Python will not find it");
            None
        }
    };

    // A provider's own picture is kept beside the home, not in the transcript.
    blobs::install(&cfg.home.join("blobs"));
    // Capped output has to go somewhere the model can read it back.
    let output_dir = cfg.home.join("outputs");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        tracing::warn!(error = %e, "cannot create {output_dir:?}; capped output will be dropped");
    }

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

    // The channels `exec` speaks to: a table of programs, not processes.
    let channels = Arc::new(crate::tools::channel::Channels::for_home(
        &cfg,
        store.clone(),
        {
            let store = store.clone();
            let capped_at = cfg.bash.max_output_chars;
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
                // Marked: the server talking, so it must not wake an idle session.
                if let Err(e) = store.add_pending_task(
                    &end.session,
                    "inject",
                    &serde_json::json!({
                        "session": end.session,
                        "message": message,
                        "notice": "channel",
                    }),
                ) {
                    tracing::warn!(
                        session = %end.session, channel = %end.id,
                        error = %e, "could not notice a finished channel"
                    );
                }
            })
        },
    ));

    let agent = Arc::new(Agent {
        store: store.clone(),
        providers: providers::Handle,
        model_config: cfg.model.clone(),
        clients: std::sync::Mutex::new(std::collections::HashMap::new()),
        default_context: cfg.model.context,
        channels: channels.clone(),
        tools: Arc::new(
            ToolRegistry::settings(
                cfg.tools.max_wait,
                cfg.bash.timeout,
                cfg.bash.max_output_chars,
            )
            .with_live_compact(cfg.agent.live_compact_enabled)
            .with_python(cfg.python.clone(), skill_lib.clone())
            .with_skills(
                tools::skill::programs(&cfg),
                skill_lib,
                cfg.bash.timeout,
                cfg.bash.max_output_chars,
            )
            .build(&cfg.tools.rows, &cfg.tools.arguments),
        ),
        bus: bus.clone(),
        compress_above_tokens: cfg.agent.compress_above_tokens,
        live_compact_enabled: cfg.agent.live_compact_enabled,
        live_compact_interval_tokens: cfg.agent.live_compact_interval_tokens,
        cache_invalid_rate: cfg.agent.cache_invalid_rate,
        cache_ttl_ms: (cfg.agent.cache_ttl_secs > 0)
            .then(|| cfg.agent.cache_ttl_secs.saturating_mul(1_000)),
        index_change_tolerance: cfg.agent.index_change_tolerance,
        context_limit_percent: cfg.agent.context_limit_percent,
        home: cfg.home.clone(),
        output_dir: output_dir.clone(),
        jobs: crate::tools::tunnel::JobRegistry::new(),
    });

    // Turns in flight and subscription actions: a stop waits for both.
    let active = Arc::new(AtomicUsize::new(0));
    // Read from the database, so a restart resumes what was asked for; the
    // actions it runs take the agent's tools.
    let subscriptions =
        crate::subscriptions::Subscriptions::load(store.clone(), bus.clone(), active.clone());

    let dispatch = Arc::new(Dispatch {
        agent,
        sessions: sessions.clone(),
        store: store.clone(),
        bus: bus.clone(),
        subscriptions: subscriptions.clone(),
        scripts: scripts.clone(),
        providers: providers::Handle,
        active: active.clone(),
        shutting_down: Arc::new(AtomicBool::new(false)),
        shutdown: CancellationToken::new(),
        // Nothing has been announced yet: the first publish states every row.
        published: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    });

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
    // What a greeting hands every client: the picture a change is diffed against.
    commands::prime_sessions(&dispatch);
    // SPEC: this start adopts the channels the last one kept; a dead one is told.
    let adopted_channels = channels.adopt().await;
    if adopted_channels > 0 {
        tracing::info!(
            adopted_channels,
            "channels the last run kept, reported gone"
        );
    }
    // Whatever was in flight when the server last stopped is owed work.
    let resumed = commands::resume_pending(&dispatch).await;
    if resumed > 0 {
        tracing::info!(resumed, "resuming turns left pending by the last run");
    }
    // Failures the last run left behind, before a client can ask about them.
    monitor::sweep(&dispatch, &cfg.home).await;
    // What the last run left running is adopted, or said to be gone.
    let owed = tools::tunnel::announce_owed(&store, &cfg, &output_dir).await;
    if owed > 0 {
        tracing::info!(owed, "reported jobs a stopped server left owed");
    }
    // Whatever the last run detached is still out there; adopt its pids now.
    let adoption_started = std::time::Instant::now();
    let adopted = detached.adopt().await;
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
            mut args: protocol::Args,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::commands::Outcome> + Send + 'a>,
        > {
            Box::pin(async move {
                let Some(dispatch) = self.0.upgrade() else {
                    return crate::commands::Outcome::err("the server is stopping");
                };
                // The caller's session fills the first argument the target declares as
                // one, and only when the call did not name it.
                if let Some(spec) = dispatch
                    .catalog()
                    .await
                    .into_iter()
                    .find(|spec| spec.name == name)
                    && let Some(argument) = spec
                        .args
                        .iter()
                        .find(|argument| argument.kind.as_deref() == Some("session"))
                {
                    args.entry(argument.name.clone())
                        .or_insert_with(|| session.to_string());
                }
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

    tracing::info!(tools = ?dispatch.agent.tools.names().join(", "), "tool registry ready");
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
        bash = ?cfg.bash.program,
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

    // A signal, or a dead window: a stop request, on the path `cancel` takes.
    let signalled = dispatch.clone();
    signal::watch(
        args.parent,
        args.parent_build.as_deref(),
        move || async move { commands::stop(&signalled).await },
    );

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
