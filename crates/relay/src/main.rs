//! The relay as a program: `gongga-relay --relay <dir> [--protocol <n>] [--build <id>]`.
//!
//! # Invariant
//!
//! `--protocol` is the pair's version, and another one is refused here, where both
//! numbers can be named, instead of starting channels whose `exit` is never written. The
//! build is identity, not compatibility: SPEC pairs one *protocol*, not one commit.

use gongga_relay::relay::{PROTOCOL, REFUSED};
use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: gongga-relay --relay <dir> [--protocol <n>] [--build <id>]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut dir = None;
    let mut build = None;
    let mut protocol_asked: Option<u32> = None;
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
            // Nothing said is a server from before this flag: read the files as always.
            Some("--protocol") => {
                match args.next().and_then(|value| value.to_str()?.parse().ok()) {
                    Some(value) => protocol_asked = Some(value),
                    None => return usage(),
                }
            }
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
    if let Some(asked) = protocol_asked
        && asked != PROTOCOL
    {
        eprintln!(
            "gongga-relay: this relay speaks protocol {PROTOCOL}, and the server that asked \
             speaks {asked}{}; a channel's `cmd` and `status` are a contract of one protocol -- \
             install that server's own relay beside it (`gongga relay` from the same build, or \
             the bundle it came from)",
            match &build {
                Some(build) => format!(" (build {build}, relay {})", protocol::BUILD),
                None => String::new(),
            }
        );
        return ExitCode::from(REFUSED);
    }
    match gongga_relay::run(Path::new(&dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gongga-relay: {e:#}");
            // A pair of two builds is what an install answers: the asking server writes
            // its own copy and asks once more.
            match e.downcast_ref::<gongga_relay::relay::OutOfDate>() {
                Some(_) => ExitCode::from(REFUSED),
                None => ExitCode::FAILURE,
            }
        }
    }
}
