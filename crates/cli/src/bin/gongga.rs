//! `gongga`: the command line, one command per process.
//!
//! See `docs/notes/cli-bin-gongga.md`.

// The package is `gongga-cli`; every reader, this bin included, calls it `cli`.
use gongga_cli as cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::parse_argv(
        "gongga",
        "Gongga harness commands: one command in, its events out",
    );
    args.check_flag_order()?;
    match &args.command {
        None => cli::list_commands(&args).await,
        Some(cli::Sub::Cmd(command)) => cli::run(&args, command).await,
    }
}
