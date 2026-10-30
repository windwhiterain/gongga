//! `gongga-tui`: the window you actually run; the composition root, nothing else.
//! See `docs/notes/tui-main.md`.

/// The name this program answers to when it is the command layer.
const COMMAND: &str = "gongga";

fn main() -> anyhow::Result<()> {
    // One program, two names. This crate carries the command layer (SPEC: the
    // window is a transparent wrapper), so `gongga` is one of the two things
    // this binary is -- and a user who has only this file still needs a
    // `gongga` for a shell to drive the server with.
    //
    // The name decides, not a flag: a shell that typed `gongga` must never be
    // handed a screen it cannot see, and someone who starts the window must not
    // have to type a command first.
    let command = named(COMMAND);
    let args = if command {
        cli::parse_argv(
            COMMAND,
            "Gongga harness commands: one command in, its events out",
        )
    } else {
        cli::parse_argv("gongga-tui", "Gongga harness terminal client")
    };
    args.check_flag_order()?;
    // A command line is the command layer's business, `serve` included: only
    // the bare invocation of the *window's* name opens the window.
    if command || args.command.is_some() {
        return cli::main(args);
    }
    gongga_tui::main(args)
}

/// Whether this process was started as `name`, which is `gongga` or `gongga.exe`.
fn named(name: &str) -> bool {
    let Some(argv0) = std::env::args_os().next() else {
        return false;
    };
    is_named(std::path::Path::new(&argv0), name)
}

/// Whether `argv0` names `name`.
///
/// # Invariant
///
/// The name is read from argv[0], not from where the file is: the same file
/// installed twice under two names is two programs, and a copy loses nothing.
fn is_named(argv0: &std::path::Path, name: &str) -> bool {
    argv0
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both platforms' spellings of one name, and the other name that must not
    /// be mistaken for it.
    #[test]
    fn the_window_answers_only_to_the_window_s_name() {
        for argv0 in [
            r"C:\Users\a\.gongga\bin\gongga",
            r"C:\Users\a\.gongga\bin\gongga.EXE",
            "/home/a/.gongga/bin/gongga",
            "gongga",
        ] {
            assert!(is_named(std::path::Path::new(argv0), "gongga"), "{argv0}");
        }
        // The stem is compared whole: not this name, and not nearly it.
        for argv0 in ["gongga-tui.exe", "gongga-server.exe", "", "gongga-tui"] {
            assert!(!is_named(std::path::Path::new(argv0), "gongga"), "{argv0}");
        }
    }
}
