//! `gongga-server`: the harness backend.

mod agent;
mod bus;
mod commands;
mod config;
mod image;
mod instance;
mod llm;
mod net;
mod prompts;
mod providers;
mod script;
mod session;
mod store;
mod tools;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use clap::Parser;
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

#[derive(Debug, Parser)]
#[command(name = "gongga-server", about = "Gongga harness server", version)]
struct Args {
    /// Path to config.toml. Defaults to `$GONGGA_HOME/config.toml`.
    #[arg(long)]
    config: Option<PathBuf>,
    /// UDP address to listen on. Overrides `[server] bind`.
    #[arg(long)]
    bind: Option<String>,
    /// SQLite path; missing dirs created, `:memory:` throwaway, beats `[server] db`.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Model name. Overrides `[llm] model`.
    #[arg(long)]
    model: Option<String>,
    /// Path to bash. Overrides `[tools.bash] program`.
    #[arg(long)]
    bash: Option<PathBuf>,
    /// Home for the database, config, logs, jobs and lock; beats `[server] home`.
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Log filter, e.g. `gongga_server=debug`.
    #[arg(long, default_value = "info")]
    log: String,
}

/// Where the CLI that can talk to this server lives.
///
/// # Invariant
///
/// The path handed to the agent is one it can actually run: sibling, then the
/// staged `target/debug`, then `PATH`, and the bare name only as a warned last
/// resort. See `docs/notes/server-main.md`.
fn cli_path() -> PathBuf {
    let name = if cfg!(windows) {
        "gongga.exe"
    } else {
        "gongga"
    };
    let mut tried = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Sibling: a release install, or target/debug.
            let sibling = dir.join(name);
            if sibling.is_file() {
                return sibling;
            }
            tried.push(sibling);
            // A staged copy in `.dev/` has the real build two levels down.
            let staged = dir.join("..").join("target").join("debug").join(name);
            if staged.is_file() {
                return staged;
            }
            tried.push(staged);
        }
    }
    if let Ok(found) = which::which(name) {
        return found;
    }
    tracing::warn!(
        ?tried,
        "cannot find the gongga CLI; the model will not be able to drive this server \
         with $GONGGA_CLI. Build it (cargo build -p gongga-cli) or put it on PATH."
    );
    PathBuf::from(name)
}

fn main() -> anyhow::Result<()> {
    // Built here, not by `#[tokio::main]`, so the drop runs after `run` closed
    // every shell. See `docs/notes/server-main.md`.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    tracing::info!("main: dropping runtime");
    drop(runtime);
    tracing::info!("main: runtime dropped");
    result
}

async fn run() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log)),
        )
        .init();

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

    let mut cfg = Config::load_with_home(args.config.as_deref(), args.home.as_deref())?;
    // Written before `--db` applies: a flag must not be baked into the new config.
    cfg.write_default_if_absent()?;
    if let Some(db) = args.db {
        cfg.db_path = config::expand(db);
    }
    // Claim the home before opening the database: one server per home.
    let _instance = instance::InstanceLock::acquire(&cfg.home)?;
    // Before any request is built: the templates come from this home's config.
    let config_path = cfg.home.join("config.toml");
    prompts::install(&config_path);
    let overridden = prompts::shared().overridden();
    tracing::info!(
        file = %config_path.display(),
        overridden = ?overridden,
        "prompt templates: the shipped defaults, plus what this home overrode"
    );

    let store = store::Store::open(&cfg.db_path)?;
    let llm = Arc::new(LlmClient::new(&cfg.llm)?);
    // Jobs live beside the database; the agent is handed the CLI, not guesses.
    let shell_env = vec![
        ("GONGGA_SERVER".to_string(), cfg.bind.to_string()),
        ("GONGGA_HOME".to_string(), cfg.home.display().to_string()),
        ("GONGGA_CLI".to_string(), cli_path().display().to_string()),
    ];
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

    // Capped output has to go somewhere the model can read it back.
    let output_dir = cfg.home.join("outputs");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        tracing::warn!(error = %e, "cannot create {output_dir:?}; capped output will be dropped");
    }

    // Scripts register commands; the directory is data, so nothing here knows one.
    let scripts = script::Scripts::new(
        cfg.scripts.dir.clone(),
        cfg.scripts.interpreter(),
        vec![
            ("GONGGA_SERVER".to_string(), cfg.bind.to_string()),
            ("GONGGA_HOME".to_string(), cfg.home.display().to_string()),
            ("GONGGA_CLI".to_string(), cli_path().display().to_string()),
        ],
    );
    if let Err(e) = std::fs::create_dir_all(&cfg.scripts.dir) {
        tracing::warn!(dir = %cfg.scripts.dir.display(), error = %e, "cannot create the scripts directory");
    }
    tracing::info!(scripts = %scripts.describe(), "script commands");
    for problem in scripts.problems().await {
        tracing::warn!(problem = %problem, "a script could not be read");
    }

    // Cyclic on purpose: the `harness` tool drives the dispatch that owns it.
    let dispatch = Arc::new_cyclic(|weak| {
        let tools = Arc::new(
            ToolRegistry::with_builtins(cfg.bash.timeout, cfg.bash.max_output_chars)
                .with_harness(weak.clone(), cfg.bash.max_output_chars),
        );
        let agent = Arc::new(Agent {
            store: store.clone(),
            llm: llm.clone(),
            providers: providers::Handle,
            llm_config: cfg.llm.clone(),
            clients: std::sync::Mutex::new(std::collections::HashMap::new()),
            default_context: cfg.llm.context,
            tools,
            bus: bus.clone(),
            compress_above_tokens: cfg.agent.compress_above_tokens,
            output_dir: output_dir.clone(),
            detached: detached.clone(),
        });
        Dispatch {
            agent,
            sessions: sessions.clone(),
            store: store.clone(),
            bus: bus.clone(),
            scripts: scripts.clone(),
            providers: providers::Handle,
            active: Arc::new(AtomicUsize::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            shutdown: CancellationToken::new(),
        }
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
        roster = ?providers::roster().path(),
        providers = providers::roster().providers().len(),
        problem = providers::roster().problem(),
        "llm roster imported"
    );
    tracing::info!(
        key = %cfg.home.join("config.toml").display(),
        "clients must present the [server] key from this file (or pass --key)"
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
    use super::{Args, cli_path};

    /// The CLI path must be runnable: a bare name is only right if it is on `PATH`.
    #[test]
    fn the_cli_path_is_something_that_can_be_run() {
        let p = cli_path();
        assert!(
            p.is_file() || which::which(&p).is_ok(),
            "cli_path gave {} which is neither a file nor on PATH",
            p.display()
        );
    }

    /// SPEC: the database path can be given at startup; parsing is the check.
    #[test]
    fn the_database_path_is_a_startup_flag() {
        use clap::Parser;
        let args = Args::parse_from(["gongga-server", "--db", "/tmp/other.db"]);
        assert_eq!(
            args.db.as_deref(),
            Some(std::path::Path::new("/tmp/other.db"))
        );
        // Without it the configured path stands, which is the default case.
        assert!(Args::parse_from(["gongga-server"]).db.is_none());
    }
}
