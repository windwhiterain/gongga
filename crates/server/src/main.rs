//! `gongga-server`: the harness backend.

mod agent;
mod bus;
mod commands;
mod config;
mod instance;
mod llm;
mod net;
mod session;
mod store;
mod tools;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::commands::Dispatch;
use crate::config::Config;
use crate::llm::LlmClient;
use crate::session::SessionManager;
use crate::tools::detached::DetachedRegistry;
use crate::tools::ToolRegistry;

#[derive(Debug, Parser)]
#[command(name = "gongga-server", about = "Gongga harness server", version)]
struct Args {
    /// Path to config.toml. Defaults to `$GONGGA_HOME/config.toml`.
    #[arg(long)]
    config: Option<PathBuf>,
    /// UDP address to listen on. Overrides `[server] bind`.
    #[arg(long)]
    bind: Option<String>,
    /// SQLite database path.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Model name. Overrides `[llm] model`.
    #[arg(long)]
    model: Option<String>,
    /// Path to bash. Overrides `[tools.bash] program`.
    #[arg(long)]
    bash: Option<PathBuf>,
    /// Log filter, e.g. `gongga_server=debug`.
    #[arg(long, default_value = "info")]
    log: String,
}

/// Where the CLI that can talk to this server lives.
///
/// The server and the client ship together, so the client is the sibling of
/// this executable -- `gongga-server` next to `gongga`. Falling back to the
/// bare name still gives the model something to type; if it is on `PATH` that
/// works too.
fn cli_path() -> PathBuf {
    let name = if cfg!(windows) { "gongga.exe" } else { "gongga" };
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from(name);
    };
    let Some(dir) = exe.parent() else {
        return PathBuf::from(name);
    };
    let sibling = dir.join(name);
    if sibling.exists() {
        sibling
    } else {
        PathBuf::from(name)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log)),
        )
        .init();

    // Command-line flags win. The simplest way to make that true without
    // threading overrides through the loader is to promote them to the same
    // environment variables the loader already consults.
    // SAFETY: this runs before any thread is spawned, so the process is
    // effectively single-threaded and no other code can observe these writes.
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

    let mut cfg = Config::load(args.config.as_deref())?;
    if let Some(db) = args.db {
        cfg.db_path = db;
    }
    cfg.write_default_if_absent()?;
    // Before touching the database: two servers on one home would each resume
    // the same pending turn and drive the same session twice.
    let _instance = instance::InstanceLock::acquire(&cfg.home)?;

    let store = store::Store::open(&cfg.db_path)?;
    let llm = Arc::new(LlmClient::new(&cfg.llm)?);
    // Detached jobs live next to the database: they outlive this process, so
    // their logs have to outlive it too.
    // How the model drives itself. `bash` runs inside this process's children,
    // so the server address and the CLI that speaks to it have to be handed
    // down rather than discovered: without them an agent that wants to cancel
    // or restart has no better option than killing its own server from outside.
    // `GONGGA_HOME` and `GONGGA_SERVER` are exactly the two fallbacks the CLI
    // reads, so `$GONGGA_CLI '<command>'` works with no flags at all.
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
    let tools = Arc::new(ToolRegistry::with_builtins(
        cfg.bash.timeout,
        cfg.bash.max_output_chars,
    ));
    let bus = EventBus::new(4096);

    // Capped output has to go somewhere the model can read it back.
    let output_dir = cfg.home.join("outputs");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        tracing::warn!(error = %e, "cannot create {output_dir:?}; capped output will be dropped");
    }

    let agent = Arc::new(Agent {
        store: store.clone(),
        llm: llm.clone(),
        tools: tools.clone(),
        bus: bus.clone(),
        max_steps: cfg.agent.max_steps,
        compress_above_chars: cfg.agent.compress_above_chars,
        keep_recent: cfg.agent.keep_recent,
        output_dir,
        detached: detached.clone(),
    });

    let dispatch = Arc::new(Dispatch {
        agent,
        sessions: sessions.clone(),
        store: store.clone(),
        bus: bus.clone(),
        active: Arc::new(AtomicUsize::new(0)),
        shutdown: CancellationToken::new(),
    });

    // Whatever was in flight when the server last stopped is owed work.
    let resumed = commands::resume_pending(&dispatch).await;
    if resumed > 0 {
        tracing::info!(resumed, "resuming turns left pending by the last run");
    }
    // So is whatever the last run detached: those processes are still out
    // there, and the only way to stop them later is to adopt their pids now.
    let adopted = detached.adopt().await;
    if adopted > 0 {
        tracing::info!(
            adopted,
            "detached jobs from the previous run are still running and can be killed"
        );
    }

    tracing::info!(tools = ?tools.names().join(", "), "tool registry ready");
    tracing::info!(
        key = %cfg.home.join("config.toml").display(),
        "clients must present the [server] key from this file (or pass --key)"
    );
    tracing::info!(
        home = %cfg.home.display(),
        db = %cfg.db_path.display(),
        model = %cfg.llm.model,
        base_url = %cfg.llm.base_url,
        bash = ?cfg.bash.program,
        proxy = ?std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("http_proxy"))
            .ok(),
        "gongga server starting"
    );

    net::serve(Arc::new(cfg), dispatch, bus).await
}
