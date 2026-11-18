//! What this program was asked for: the client's flags and the page's address.
//!
//! See `docs/notes/webui-args.md`.

use std::net::SocketAddr;

use anyhow::Context;
use clap::{Arg, CommandFactory, FromArgMatches};

use client::Args;

/// The command line, resolved once.
pub struct Parsed {
    /// The connection flags `gongga-client` also answers to.
    pub client: Args,
    /// The address the page is served at.
    pub bind: SocketAddr,
}

/// Reads the command line, refusing what a browser front-end may not be.
///
/// # Invariant
///
/// The page drives the harness, so it is served on loopback only: a control surface
/// with no authentication must not answer another machine. See `docs/notes/webui-server.md`.
pub fn parse() -> anyhow::Result<Parsed> {
    let matches = <Args as CommandFactory>::command()
        .name("gongga-webui")
        .bin_name("gongga-webui")
        .about("Gongga harness browser front-end: one page, gongga-client over a pipe")
        .version(client::protocol::version_line())
        .arg(
            Arg::new("bind")
                .long("bind")
                .value_name("ADDRESS")
                .default_value("127.0.0.1:9701")
                .help("Address the page is served at; loopback only"),
        )
        .arg(
            Arg::new("instance")
                .long("instance")
                .value_name("NAME")
                .help("Keep this window's settings in <client-home>/<NAME>"),
        )
        .get_matches();
    let mut client = <Args as FromArgMatches>::from_arg_matches(&matches)
        .context("reading the client's own flags")?;
    let bind = matches
        .get_one::<String>("bind")
        .map(String::as_str)
        .unwrap_or("127.0.0.1:9701");
    let bind: SocketAddr = bind
        .parse()
        .with_context(|| format!("--bind is not an address: {bind}"))?;
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "the page drives the harness, so it is served on loopback only"
    );
    if let Some(name) = matches.get_one::<String>("instance") {
        anyhow::ensure!(
            plain_name(name),
            "--instance takes a plain name, not a path"
        );
        let base = client::client_home_of(&client);
        client.conn.client_home = Some(base.join(name));
    }
    anyhow::ensure!(client.command.is_none(), "a command belongs to gongga-cli");
    Ok(Parsed { client, bind })
}

/// Whether a name may be one directory under the client home.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
}
