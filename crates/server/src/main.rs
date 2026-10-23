//! `gongga-server`: the harness backend.

mod agent;
mod bus;
mod commands;
mod config;
mod instance;
mod llm;
mod net;
mod providers;
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
    /// SQLite database path. Overrides `[server] db`.
    ///
    /// Missing directories are created, and `:memory:` gives a throwaway
    /// database — which is what makes a clean end-to-end run possible without
    /// touching the operator's real one.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Model name. Overrides `[llm] model`.
    #[arg(long)]
    model: Option<String>,
    /// Path to bash. Overrides `[tools.bash] program`.
    #[arg(long)]
    bash: Option<PathBuf>,
    /// Home directory: the database, config, logs, detached-job records and
    /// the instance lock all live under it. Overrides `[server] home`, which
    /// overrides `$GONGGA_HOME`.
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Log filter, e.g. `gongga_server=debug`.
    #[arg(long, default_value = "info")]
    log: String,
}

/// Where the CLI that can talk to this server lives.
///
/// The server and the client ship together, so the usual answer is the sibling
/// of this executable. That is not the only layout in use, though: `dev.sh`
/// copies the server into `.dev/` *alone*, so the sibling is missing there and
/// the agent ends up with a `GONGGA_CLI` that is not on `PATH` -- the one thing
/// it needs in order to drive (or replace) the server it is running on.
///
/// So: sibling, then the `target/debug` a staged copy sits under, then `PATH`,
/// and only as a last resort the bare name -- announced, because a bare name
/// that is not on `PATH` is a dead end the agent cannot diagnose.
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
    // The runtime is built here rather than by `#[tokio::main]` so that the
    // drop happens at a point where `run` has already closed every shell. The
    // drop still waits, with no deadline -- but it now waits for something that
    // ends, which is the difference between waiting and hanging.
    //
    // The bug this replaces: every blocking task had to drain before the
    // process could go, one of them was the reaper for a persistent shell, and
    // a persistent shell never exits on its own. After a session had used
    // `bash`, the process sat there having already logged `server stopped`.
    // Putting a 2s deadline on the drain hid that instead of fixing it; what
    // fixes it is ending the shells in `run`, after which there is nothing left
    // for the drain to wait on.
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

    let mut cfg = Config::load_with_home(args.config.as_deref(), args.home.as_deref())?;
    // The starter config is written from what the *file* said, before the flag
    // is applied: `--db /tmp/x.db` is this run's database, and baking it into a
    // config.toml that this run happened to create would quietly move every
    // later run to that path as well.
    cfg.write_default_if_absent()?;
    if let Some(db) = args.db {
        cfg.db_path = config::expand(db);
    }
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
        providers: providers::roster(),
        llm_config: cfg.llm.clone(),
        clients: std::sync::Mutex::new(std::collections::HashMap::new()),
        default_context: cfg.llm.context,
        tools: tools.clone(),
        bus: bus.clone(),
        compress_above_tokens: cfg.agent.compress_above_tokens,
        keep_recent: cfg.agent.keep_recent,
        output_dir,
        detached: detached.clone(),
    });

    let dispatch = Arc::new(Dispatch {
        agent,
        sessions: sessions.clone(),
        store: store.clone(),
        bus: bus.clone(),
        providers: providers::roster(),
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
    let adoption_started = std::time::Instant::now();
    let adopted = detached.adopt().await;
    // The duration is part of the message because adoption has to finish
    // before the port is bound: a home with thousands of rows looked exactly
    // like a server that would not start, and there was nothing to point at.
    // Logged unconditionally, `adopted = 0` included -- the run that took 75
    // seconds to bind had 156 stale rows and nothing to adopt, so the one line
    // that would have said so was the one the condition suppressed.
    tracing::info!(
        adopted,
        elapsed_ms = adoption_started.elapsed().as_millis() as u64,
        "detached jobs adopted from the previous run"
    );

    // A message that arrived while a turn was running is normally folded into
    // that turn at its next step boundary. If the turn ends first, nobody is
    // left to read it -- so a session holding an undelivered message gets a
    // turn started for it. Checked rather than pushed: the row is durable, so
    // a miss here costs a couple of seconds, not the message.
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

    tracing::info!(tools = ?tools.names().join(", "), "tool registry ready");
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
    // Every path out of `serve` ends here, so this is the one place that
    // guarantees no shell outlives the server's decision to stop.
    //
    // It has to happen before the runtime goes: a live persistent shell is a
    // child that never exits on its own, and the thread reaping it can never
    // finish while it lives -- which is exactly the state that made the process
    // sit there after `server stopped`. Ending the shells is what gives those
    // threads an end, so the runtime's `Drop` can simply wait for them instead
    // of being given a deadline.
    sessions.close_shells().await;
    served
}

#[cfg(test)]
mod tests {
    use super::{Args, cli_path};

    /// The CLI is what lets the model cancel, restart or replace this server,
    /// so the path it is handed has to be runnable -- a bare name is only
    /// correct if that name is actually on `PATH`.
    #[test]
    fn the_cli_path_is_something_that_can_be_run() {
        let p = cli_path();
        assert!(
            p.is_file() || which::which(&p).is_ok(),
            "cli_path gave {} which is neither a file nor on PATH",
            p.display()
        );
    }

    /// SPEC: "database path can be specified at server start." Checked by
    /// parsing rather than by reading the source, so that renaming the flag
    /// breaks a test instead of someone's runbook.
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
