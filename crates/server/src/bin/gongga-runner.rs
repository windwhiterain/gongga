//! `gongga-runner`: the program a peer runs to own that machine's shells.
//!
//! See `docs/notes/server-remote.md`.

fn main() -> anyhow::Result<()> {
    // Built here, not by `#[tokio::main]`: the runtime drops where teardown shows.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(gongga_server::remote::runner::serve_stdio());
    drop(runtime);
    result
}
