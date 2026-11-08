//! `gongga-server`: the harness backend, as a program of its own.
//!
//! See `docs/notes/server-main.md`.

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches};

use gongga_server::{Args, LogFormat};

#[derive(Debug, clap::Parser)]
#[command(name = "gongga-server", about = "Gongga harness server")]
struct Command {
    /// Path to config.toml. Defaults to `$GONGGA_HOME/config.toml`.
    #[arg(long)]
    config: Option<PathBuf>,
    /// UDP address to listen on. Overrides `[server] bind`.
    #[arg(long)]
    bind: Option<String>,
    /// Model name. Overrides `[provider] model`.
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
    /// How each log line is written: text for a reader, json for a tool.
    #[arg(long, value_enum, default_value = "text")]
    log_format: LogFormat,
    /// Stop when this process ends: the window that started this server.
    #[arg(long, value_name = "PID")]
    parent: Option<u32>,
    /// The build of that window, so one this build cannot serve is not followed.
    #[arg(long, value_name = "BUILD")]
    parent_build: Option<String>,
    /// Run one tool call and write its answer: how a detached call outlives the server.
    #[arg(long, value_name = "FILE", hide = true)]
    run_tool: Option<PathBuf>,
}

impl From<Command> for Args {
    fn from(c: Command) -> Self {
        Self {
            config: c.config,
            bind: c.bind,
            model: c.model,
            bash: c.bash,
            home: c.home,
            log: c.log,
            log_format: c.log_format,
            parent: c.parent,
            parent_build: c.parent_build,
        }
    }
}

/// The words the user typed, which `gongga server --help` has to answer with.
fn spoken_name() -> Option<String> {
    std::env::var("GONGGA_NAME")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn command() -> clap::Command {
    match spoken_name() {
        // `name` wants a `'static` str; this runs once, for the life of the process.
        Some(name) => {
            let name: &'static str = Box::leak(name.into_boxed_str());
            Command::command().name(name).bin_name(name)
        }
        None => Command::command(),
    }
}

fn main() -> anyhow::Result<()> {
    // SPEC's watchdog is this binary watching this binary; a question skips it.
    if !gongga_server::supervise::is_child() && !gongga_server::supervise::answers_alone() {
        std::process::exit(gongga_server::supervise::watch());
    }
    // Carries the build, which clap cannot know, and answers before any home.
    if std::env::args_os().any(|arg| arg == "--version" || arg == "-V") {
        // The same line the other two print: crate, version, build.
        println!("gongga-server {}", protocol::version_line());
        return Ok(());
    }
    let matches = command().get_matches();
    let command =
        Command::from_arg_matches(&matches).expect("clap already rejected what it could not parse");
    // Before anything of a server: this process is one call, and answers with a file.
    if let Some(request) = &command.run_tool {
        return gongga_server::worker::run(request);
    }
    // A failure here is an explanation, not a crash: exit 2, which is not retried.
    match gongga_server::main(command.into()) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(gongga_server::supervise::REFUSED);
        }
    }
}
