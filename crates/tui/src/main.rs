//! `gongga-tui`: the window you actually run; the composition root, nothing else.
//! See `docs/notes/tui-main.md`.

fn main() -> anyhow::Result<()> {
    let args = cli::parse_argv("gongga-tui", "Gongga harness terminal client");
    args.check_flag_order()?;
    // A command line is the command layer's business, `serve` included: only
    // the bare invocation opens the window.
    if args.command.is_some() {
        return cli::main(args);
    }
    gongga_tui::main(args)
}
