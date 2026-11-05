//! `gongga-cli`: the command line, one command per process.
//!
//! See `docs/notes/cli-bin-gongga.md`.

// The package is `gongga-cli`; what a command line means lives in `link`, the
// same library the window is built on.
fn main() -> anyhow::Result<()> {
    let args = link::parse_argv(link::spoken_name(), link::ABOUT);
    args.check_flag_order()?;
    link::main(args)
}
