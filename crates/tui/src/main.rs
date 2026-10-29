//! `gongga-tui`: the window you actually run; the composition root, nothing else.
//! See `docs/notes/tui-main.md`.

use cli::Sub;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::parse_argv("gongga-tui", "Gongga harness terminal client");
    args.check_flag_order()?;
    match &args.command {
        None => gongga_tui::interactive(&args).await,
        Some(Sub::Cmd(command)) => cli::run(&args, command).await,
    }
}
