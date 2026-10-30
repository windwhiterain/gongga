//! `gongga-tui`: the window you actually run; the composition root, nothing else.
//! See `docs/notes/tui-main.md`.

/// The name and the sentence this file answers with, which depend on the name it
/// was started under: the same file is both halves of one program.
fn describe() -> (&'static str, &'static str) {
    if named(cli::COMMAND) {
        (cli::COMMAND, cli::ABOUT)
    } else {
        ("gongga-tui", "Gongga harness terminal client")
    }
}

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

/// Whether this process was started under `name`.
///
/// # Invariant
///
/// The file name, not a flag: `<home>/bin/gongga` is this same program
/// (`cli-server.md`), so the name is what tells the two halves apart -- and it
/// has to, because a link has no room for an argument.
fn named(name: &str) -> bool {
    let Some(argv0) = std::env::args_os().next() else {
        return false;
    };
    is_name(std::path::Path::new(&argv0), name)
}

/// The comparison itself, so a test does not have to be argv[0].
fn is_name(path: &std::path::Path, name: &str) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case(name))
}

/// Takes the leading `cli` off a command line, and says whether it was there.
///
/// # Invariant
///
/// `gongga-tui cli` with nothing after it is the catalog, exactly like `gongga`
/// with nothing at all: the word is removed with the command left empty, not with
/// a command named `cli` -- and not with the window, which is what an empty
/// command line means everywhere else.
fn took_layer(args: &mut cli::Args) -> bool {
    let Some(cli::Sub::Cmd(tokens)) = &mut args.command else {
        return false;
    };
    if tokens.first().map(String::as_str) != Some(cli::CLI) {
        return false;
    }
    tokens.remove(0);
    if tokens.is_empty() {
        args.command = None;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command line as this crate receives it after clap.
    fn args(words: &[&str]) -> cli::Args {
        cli::Args {
            conn: cli::Connection {
                server: None,
                home: None,
                key: None,
            },
            client_only: false,
            command: (!words.is_empty()).then(|| {
                cli::Sub::Cmd(words.iter().map(|word| word.to_string()).collect())
            }),
        }
    }

    fn command(args: &cli::Args) -> Option<Vec<String>> {
        match &args.command {
            Some(cli::Sub::Cmd(tokens)) => Some(tokens.clone()),
            None => None,
        }
    }

    /// The word is a way of naming a half of this program, so what is left is the
    /// command it was hiding.
    #[test]
    fn the_layer_s_word_is_taken_off_and_the_command_stays() {
        let mut args = args(&["cli", "get_messages", "--session", "main"]);
        assert!(took_layer(&mut args));
        assert_eq!(
            command(&args),
            Some(vec![
                "get_messages".to_string(),
                "--session".to_string(),
                "main".to_string()
            ])
        );
    }

    /// Alone, it means the catalog -- not a command called `cli`, and not the
    /// window, which is what an empty command line means.
    #[test]
    fn the_layer_s_word_alone_leaves_a_command_line_with_no_command() {
        let mut args = args(&["cli"]);
        assert!(took_layer(&mut args));
        assert_eq!(command(&args), None);
    }

    /// A command line that never said it is passed on untouched: `gongga-tui
    /// serve` and `gongga-tui get_messages` are still the layer's business.
    #[test]
    fn a_command_line_without_the_word_is_left_alone() {
        for words in [&["get_messages", "--session", "main"], &["serve", "--port", "9700"]] {
            let mut args = args(words);
            let before = command(&args);
            assert!(!took_layer(&mut args), "{words:?}");
            assert_eq!(command(&args), before, "{words:?}");
        }
        // Not even a command that merely starts with it.
        let mut args = self::args(&["cli-other"]);
        assert!(!took_layer(&mut args));
    }

    /// The name is the whole difference: the same file installed as `gongga` is
    /// the command layer, under its own name it is the window.
    #[test]
    fn the_command_s_name_is_recognised_in_either_spelling() {
        for (path, expected) in [
            ("gongga", true),
            ("/home/a/.gongga/bin/gongga", true),
            (r"C:\Users\a\.gongga\bin\gongga.exe", true),
            (r"C:\Users\a\.gongga\bin\GONGGA.EXE", true),
            ("gongga-tui", false),
            ("gongga-tui.exe", false),
            ("", false),
        ] {
            let path = std::path::Path::new(path);
            assert_eq!(is_name(path, cli::COMMAND), expected, "{path:?}");
        }
    }
}
