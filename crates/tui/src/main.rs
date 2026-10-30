//! `gongga-tui`: the window you actually run; the composition root, nothing else.
//! See `docs/notes/tui-main.md`.

use gongga_tui::program::{describe, named, took_layer};

fn main() -> anyhow::Result<()> {
    let mut args = cli::parse_argv(describe().0, describe().1);
    // `cli` is this program's own name for the layer it carries: a shell that was
    // started by a window has one program to type, so it may say which half it
    // means. Under the name `gongga` it does not even have to.
    let layer = took_layer(&mut args);
    args.check_flag_order()?;
    // A command line is the command layer's business, `serve` included: only the
    // bare invocation is the window, and only under this program's own name.
    if layer || args.command.is_some() || named(cli::COMMAND) {
        return cli::main(args);
    }
    gongga_tui::main(args)
}
