//! `gongga-server`: the harness backend, as a library.
//!
//! See `docs/notes/server-lib.md`.

mod agent;
mod agents;
mod blobs;
mod body;
mod bus;
mod commands;
mod config;
mod image;
mod instance;
mod llm;
mod migrate;
mod net;
mod probe;
mod prompts;
mod providers;
mod script;
mod session;
mod shell_command;
mod ship;
mod signal;
mod store;
mod tools;
mod transcript;
mod transport;

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
    /// Model name. Overrides `[llm] model`.
    pub model: Option<String>,
    /// Path to bash. Overrides `[tools.bash] program`.
    pub bash: Option<PathBuf>,
    /// Home for the database, config, logs, jobs and lock; beats `[server] home`.
    pub home: Option<PathBuf>,
    /// Log filter, e.g. `gongga_server=debug`.
    pub log: String,
    /// How each log line is written: text for a reader, json for a tool.
    pub log_format: LogFormat,
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
        }
    }
}

/// The shape of one log line.
///
/// `json` exists because everything a benchmark wants to know -- a step, a
/// request's latency, a retry, a turn's outcome -- was already in the log and
/// only reachable by matching prose that carries colour escapes. A tool should
/// never have to parse that.
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
        // Colour only for a terminal. Written to a file, the escapes landed
        // inside the tokens, and a reader matching `outcome=Done` saw a
        // finished turn as one that never ended.
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

    let cfg = config_from(&args)?;
    // Claim the home before opening the database: one server per home.
    let _instance = instance::InstanceLock::acquire(&cfg.home)?;
    // Off this thread before anything reads a roster: the community copy may not
    // be here yet, and no start waits for a network. See `docs/notes/server-agents.md`.
    agents::start();
    // Before anything reads this home: one migrator per version step, and only
    // for a home that is behind.
    if migrate::needs_content(&cfg.home, &cfg.db_path) {
        ship::prepare_for_migration(&cfg.home);
    } else {
        ship::prepare(&cfg.home);
    }
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
        pin = %installed.pin.get(..10).unwrap_or(installed.pin.as_str()),
        written = installed.written,
        updated = installed.updated,
        merged = installed.merged,
        removed = installed.removed,
        conflicts = ?installed.conflicts,
        fetched = installed.fetched,
        "shipped content"
    );
    // Read again: the install may have just placed this home's config.
    let cfg = config_from(&args)?;
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
    let llm = Arc::new(LlmClient::new(&cfg.llm)?);
    // A script drives this server by the same name a shell does: `gongga`, which
    // is this program under the command's name in the home's `bin`.
    // Before any shell exists: the command it types has to be there, and a
    // server started by something other than the command layer has none beside
    // it. Best effort -- a server with no command still serves, and says so.
    let command = shell_command::ensure_command(&cfg.home);
    if command.is_none() {
        shell_command::warn_missing_command(&cfg.home);
    }
    let mut base_env = vec![
        ("GONGGA_SERVER".to_string(), cfg.bind.to_string()),
        ("GONGGA_HOME".to_string(), cfg.home.display().to_string()),
    ];
    // Named and absolute, so a script or a launcher does not have to resolve it
    // through a `PATH` that a shell may have rewritten.
    if let Some(command) = &command {
        base_env.push((
            "GONGGA_CLI".to_string(),
            command.to_string_lossy().into_owned(),
        ));
    }
    // A script's interpreter reads a native `PATH`; a shell this server starts
    // reads a POSIX one. Both begin with the same directory.
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
        cfg.bash.clone(),
        cfg.llm.model.clone(),
        detached.clone(),
        shell_env,
    );
    let bus = EventBus::new(4096);

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

    let dispatch = Arc::new(Dispatch {
        agent: Arc::new(Agent {
            store: store.clone(),
            llm: llm.clone(),
            providers: providers::Handle,
            llm_config: cfg.llm.clone(),
            clients: std::sync::Mutex::new(std::collections::HashMap::new()),
            default_context: cfg.llm.context,
            tools: Arc::new(ToolRegistry::with_builtins(
                cfg.bash.timeout,
                cfg.bash.max_output_chars,
            )),
            bus: bus.clone(),
            compress_above_tokens: cfg.agent.compress_above_tokens,
            output_dir: output_dir.clone(),
            detached: detached.clone(),
        }),
        sessions: sessions.clone(),
        store: store.clone(),
        bus: bus.clone(),
        scripts: scripts.clone(),
        providers: providers::Handle,
        active: Arc::new(AtomicUsize::new(0)),
        shutting_down: Arc::new(AtomicBool::new(false)),
        shutdown: CancellationToken::new(),
    });

    // Whatever was in flight when the server last stopped is owed work.
    let resumed = commands::resume_pending(&dispatch).await;
    if resumed > 0 {
        tracing::info!(resumed, "resuming turns left pending by the last run");
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

    // A session holding an undelivered message gets a turn started for it.
    let wake = dispatch.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            let started = commands::nudge_inboxes(&wake);
            if started > 0 {
                tracing::info!(started, "woke sessions holding an undelivered message");
            }
        }
    });

    tracing::info!(tools = ?dispatch.agent.tools.names().join(", "), "tool registry ready");
    // Which roster was read, and whether it was readable at all: "no --llm
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
        db = %cfg.db_path.display(),
        provider = ?cfg.llm.provider,
        model = %cfg.llm.model,
        base_url = %cfg.llm.base_url,
        context = ?cfg.llm.context,
        bash = ?cfg.bash.program,
        proxy = ?std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("http_proxy"))
            .ok(),
        "gongga server starting"
    );

    // A signal is a stop request, not a kill: the same path as `cancel`.
    let signalled = dispatch.clone();
    signal::watch(move || async move { commands::stop(&signalled).await });

    let served = net::serve(Arc::new(cfg), dispatch, bus).await;
    // Every exit from `serve` lands here, so nothing it started outlives the
    // stop decision. See `docs/notes/server-main.md`.
    sessions.close_shells().await;
    let stopped = scripts.stop_all().await;
    if stopped > 0 {
        tracing::info!(stopped, "stopped scripts still running at the stop");
    }
    served
}

#[cfg(test)]
mod tests {
    use super::Args;

    /// SPEC: the home can be named at server start, and `serve` is what names
    /// it -- so an empty `Args` has to mean "whatever the home's config says".
    #[test]
    fn an_empty_start_asks_for_nothing() {
        let args = Args::default();
        assert!(args.home.is_none());
        assert!(args.bind.is_none());
        assert_eq!(args.log, "info");
    }
}
