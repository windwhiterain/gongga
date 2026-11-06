//! One tool call, run by a process of its own.
//!
//! See `docs/notes/server-tools-worker.md`.

use std::sync::Arc;

use serde_json::Value;

/// What the server writes before it starts a worker.
///
/// # Invariant
///
/// Everything the call needs is in this file: a worker reads no state of the server
/// that started it, which is the whole point -- it is still there when that server
/// is not.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Request {
    /// The home this call belongs to: its config, its database, its output directory.
    pub home: std::path::PathBuf,
    pub session: String,
    pub call_id: String,
    pub tool: String,
    pub args: Value,
    pub cwd: std::path::PathBuf,
    pub env: Vec<(String, String)>,
    /// Where the answer goes: the server reads it, now or after a restart.
    pub answer: std::path::PathBuf,
    /// Whether the session this call belongs to is for a read.
    ///
    /// # Invariant
    ///
    /// Carried in the file because a worker keeps no state of the server that wrote it:
    /// a mode the request does not spell is a way around a refusal the caller's own
    /// process makes. The look a call ends with is not taken here -- the server that
    /// collects the answer takes it, on the session it knows. See
    /// `docs/notes/server-explore.md`.
    #[serde(default)]
    pub explore: bool,
}

/// What a worker leaves behind for the server to collect.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Answer {
    pub ok: bool,
    pub output: String,
}

/// Runs the call one request names, and writes its answer where it was told.
///
/// # Invariant
///
/// The answer is written by a rename, so a reader never sees half of one: the server
/// reads this file precisely when it cannot ask the worker anything.
pub fn run(request_path: &std::path::Path) -> anyhow::Result<()> {
    let request: Request = serde_json::from_slice(&std::fs::read(request_path)?)?;
    let answer = answer_of(&request);
    let text = serde_json::to_vec(&answer)?;
    let writing = request.answer.with_extension("writing");
    std::fs::write(&writing, &text)?;
    std::fs::rename(&writing, &request.answer)?;
    Ok(())
}

/// The answer a worker left, if it left one.
pub fn read(path: &std::path::Path) -> Option<Answer> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn answer_of(request: &Request) -> Answer {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            return Answer {
                ok: false,
                output: format!("the worker could not start a runtime: {e}"),
            };
        }
    };
    runtime.block_on(call(request))
}

/// Builds what one call needs and runs it, with no session and no turn behind it.
async fn call(request: &Request) -> Answer {
    let cfg = match crate::config::Config::load_with_home(
        Some(&request.home.join("config.toml")),
        Some(&request.home),
    ) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Answer {
                ok: false,
                output: format!("this home's config could not be read: {e}"),
            };
        }
    };
    // A worker is not the server: an unreadable database costs the failure notes.
    let store = crate::store::Store::open(&request.home.join("gongga.db"))
        .or_else(|_| crate::store::Store::open(std::path::Path::new(":memory:")));
    let store = match store {
        Ok(store) => store,
        Err(e) => {
            return Answer {
                ok: false,
                output: format!("the worker could not open this home's database: {e}"),
            };
        }
    };
    let lib = crate::pylib::install(&request.home).ok();
    let tools = Arc::new(
        crate::tools::ToolRegistry::with_builtins(
            cfg.tools.max_wait,
            cfg.bash.timeout,
            cfg.bash.max_output_chars,
        )
        .with_python(cfg.python.clone(), lib.clone())
        .with_skills(
            crate::tools::skill::programs(&cfg),
            lib,
            cfg.bash.timeout,
            cfg.bash.max_output_chars,
        ),
    );
    let program = cfg.bash.program.clone().unwrap_or_default();
    let ctx = crate::tools::ToolContext {
        session: request.session.clone(),
        call_id: request.call_id.clone(),
        home: request.home.clone(),
        output_dir: request.home.join("outputs"),
        // No shells, no interpreters, no jobs: one call, and the tools that need none.
        shells: Arc::new(std::sync::Mutex::new(crate::session::Shells::empty(
            program,
        ))),
        pythons: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        reader: None,
        machine: crate::session::Machine::Local,
        remote: None,
        detached: crate::tools::detached::DetachedRegistry::new(
            store.clone(),
            cfg.bash.program.clone(),
            request.home.join("detached"),
            request.env.clone(),
        ),
        jobs: crate::tools::tunnel::JobRegistry::new(),
        events: None,
        cancel: tokio_util::sync::CancellationToken::new(),
        store,
        cwd: request.cwd.clone(),
        env: request.env.clone(),
        vision: crate::llm::Vision {
            accepts_image: false,
            refusal: "a detached call has no provider to send a picture to".to_string(),
        },
        inbox: crate::session::InboxSignal::never(),
        holds_the_turn: false,
        stop: crate::session::StopSignal::idle(),
        closing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        // Its own, so a write-shaped tool is refused here too.
        explore: request.explore.then(|| {
            crate::explore::watch_of(crate::explore::Mode::Explore)
                .unwrap_or_else(|| std::sync::Arc::new(crate::explore::Watch::default()))
        }),
    };
    let outcome = tools.run(&request.tool, request.args.clone(), &ctx).await;
    Answer {
        ok: outcome.ok,
        output: outcome.output,
    }
}
