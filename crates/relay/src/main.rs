//! The relay as a program: `gongga-relay --relay <dir>`.
//!
//! The flag is the one a server names when it starts a channel, so a channel recorded
//! by an older build still starts: nothing about this program is discovered by argument
//! shape, only by the directory it is handed.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(flag) = args.next() else {
        eprintln!("usage: gongga-relay --relay <dir>");
        return ExitCode::from(2);
    };
    let Some(dir) = args.next() else {
        eprintln!("usage: gongga-relay --relay <dir>");
        return ExitCode::from(2);
    };
    if flag != "--relay" {
        eprintln!("usage: gongga-relay --relay <dir>");
        return ExitCode::from(2);
    }
    match gongga_relay::run(Path::new(&dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gongga-relay: {e:#}");
            ExitCode::FAILURE
        }
    }
}
