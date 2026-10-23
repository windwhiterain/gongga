//! `gongga`: the client you actually run.
//!
//! Two shapes, one command line: with a command it is headless (JSON lines,
//! one command per process, which is how scripts and the model drive the
//! harness); with none it opens the window.
//!
//! This file is the composition root and nothing else. It parses the command
//! line the way the command layer defines it and hands it to the layer that
//! knows what to do with it -- the interactive loop for "no command", the
//! command layer's one-shot runner otherwise. It deliberately holds no command
//! knowledge of its own: adding a command must not mean touching this crate,
//! which is what "transparent wrapper" means here.

use cli::Sub;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::parse_argv();
    args.check_flag_order()?;
    match &args.command {
        None => gongga_tui::interactive(&args).await,
        Some(Sub::Cmd(command)) => cli::run(&args, command).await,
    }
}
