//! `gongga-tui`: the window you actually run; the composition root, nothing else.
//! See `docs/notes/tui-main.md`.

fn main() -> anyhow::Result<()> {
    let mut args = cli::parse_argv("gongga-tui", "Gongga harness terminal client");
    // `cli` is this program's own name for the layer it carries: a shell that was
    // started by a window has one program to type, so it says which half it means.
    let layer = took_layer(&mut args);
    args.check_flag_order()?;
    // A command line is the command layer's business, `serve` included: only the
    // bare invocation opens the window.
    if layer || args.command.is_some() {
        return cli::main(args, Some(cli::CLI));
    }
    gongga_tui::main(args)
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
}
