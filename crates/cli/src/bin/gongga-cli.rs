//! `gongga-cli`: the command line, one command per process.
//!
//! See `docs/notes/cli-bin-gongga.md`.

// The package is `gongga-cli`; every reader, this bin included, calls it `cli`.
use gongga_cli as cli;

fn main() -> anyhow::Result<()> {
    let args = cli::parse_argv(cli::COMMAND, cli::ABOUT);
    args.check_flag_order()?;
    cli::main(args)
}
