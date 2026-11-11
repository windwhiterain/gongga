//! The relay as a program: `gongga-relay --relay <dir> [--build <hash>]`.
//!
//! The flag is the one a server names when it starts a channel, so a channel recorded
//! by an older build still starts: nothing about this program is discovered by argument
//! shape, only by the directory it is handed.
//!
//! # Invariant
//!
//! `--build` is the pair's version. The files a relay and a server exchange (`cmd`,
//! `status`) are a contract, and a mismatched pair does not fail loudly -- it starts
//! channels whose `exit` is never written. A build that is not this one is refused here,
//! where both numbers can be named, rather than inside a channel that is already gone.

use std::path::Path;
use std::process::ExitCode;

/// The code a refusal uses: not a program that failed, a pair that cannot work.
const REFUSED: u8 = 3;

fn usage() -> ExitCode {
    eprintln!("usage: gongga-relay --relay <dir> [--build <hash>]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut dir = None;
    let mut build = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--version") | Some("-V") => {
                println!("gongga-relay {}", protocol::version_line());
                return ExitCode::SUCCESS;
            }
            Some("--relay") => match args.next() {
                Some(value) => dir = Some(std::path::PathBuf::from(value)),
                None => return usage(),
            },
            Some("--build") => match args.next() {
                Some(value) => build = Some(value.to_string_lossy().into_owned()),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(dir) = dir else {
        return usage();
    };
    if let Some(asked) = build
        && asked != protocol::BUILD
    {
        eprintln!(
            "gongga-relay: this is build {}, and the server that asked is {asked}; \
             a channel is started by a pair of the same build -- install the server's own \
             relay beside it, or `gongga relay` from the same build",
            protocol::BUILD
        );
        return ExitCode::from(REFUSED);
    }
    match gongga_relay::run(Path::new(&dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gongga-relay: {e:#}");
            ExitCode::FAILURE
        }
    }
}
