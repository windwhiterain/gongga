//! Which half of `gongga-tui` this process is, and the word that says so.
//!
//! See `docs/notes/tui-program.md`.

/// The name and the sentence to answer with: what was typed, then argv[0].
pub fn describe() -> (&'static str, &'static str) {
    if std::env::var_os("GONGGA_NAME").is_some() || named(client::COMMAND) {
        (client::spoken_name(), client::ABOUT)
    } else {
        ("gongga-tui", "Gongga harness terminal client")
    }
}

/// Whether this process was started under `name`.
///
/// # Invariant
///
/// The file name, not a flag: `<home>/bin/gongga` is this same program
/// (`cli-server.md`), so the name is what tells the two halves apart -- and it
/// has to, because a link has no room for an argument.
pub fn named(name: &str) -> bool {
    let Some(argv0) = std::env::args_os().next() else {
        return false;
    };
    is_name(std::path::Path::new(&argv0), name)
}

/// The comparison itself, so a test does not have to be argv[0].
pub fn is_name(path: &std::path::Path, name: &str) -> bool {
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
pub fn took_layer(args: &mut client::Args) -> bool {
    let Some(client::Sub::Cmd(tokens)) = &mut args.command else {
        return false;
    };
    if tokens.first().map(String::as_str) != Some(client::CLI) {
        return false;
    }
    tokens.remove(0);
    if tokens.is_empty() {
        args.command = None;
    }
    true
}
