//! `gongga-server`: the harness backend, as a program of its own.
//!
//! See `docs/notes/server-main.md`.

use std::path::PathBuf;

use clap::Parser;

use gongga_server::{Args, LogFormat};

#[derive(Debug, Parser)]
#[command(name = "gongga-server", about = "Gongga harness server", version)]
struct Command {
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
    /// How each log line is written: text for a reader, json for a tool.
    #[arg(long, value_enum, default_value = "text")]
    log_format: LogFormat,
}

impl From<Command> for Args {
    fn from(c: Command) -> Self {
        Self {
            config: c.config,
            bind: c.bind,
            db: c.db,
            model: c.model,
            bash: c.bash,
            home: c.home,
            log: c.log,
            log_format: c.log_format,
        }
    }
}

fn main() -> anyhow::Result<()> {
    gongga_server::main(Command::parse().into())
}
