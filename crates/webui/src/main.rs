//! `gongga-webui`: the browser front-end, one page at a time.
//!
//! See `docs/notes/webui-main.md`.

mod args;
mod page;
mod server;
mod window;

use std::sync::Arc;

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let parsed = args::parse()?;
    let bind = parsed.bind;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    let local = listener.local_addr().context("reading the bound address")?;
    let state = Arc::new(server::State::new(parsed, local));
    println!("gongga-webui: http://{local}/");
    tokio::select! {
        result = server::serve(listener, state.clone()) => result,
        _ = tokio::signal::ctrl_c() => {
            state.stop();
            state.idle().await;
            Ok(())
        }
    }
}
