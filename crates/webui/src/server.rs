//! The page's own HTTP and WebSocket server: one window, served on loopback.
//!
//! See `docs/notes/webui-server.md`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE, HOST, ORIGIN, UPGRADE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::args::Parsed;
use crate::page;
use crate::window;

/// The one page a process serves: a browser window claimed it, or the next may.
pub struct Slot {
    taken: AtomicBool,
    changed: watch::Sender<bool>,
    shutdown: watch::Sender<bool>,
}

impl Slot {
    fn new() -> Self {
        let (changed, _) = watch::channel(false);
        let (shutdown, _) = watch::channel(false);
        Self {
            taken: AtomicBool::new(false),
            changed,
            shutdown,
        }
    }

    /// Takes the slot, or says a page already has it.
    pub fn claim(&self) -> bool {
        if self
            .taken
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        self.changed.send_replace(true);
        true
    }

    /// Says the page is gone, so the next one may attach.
    pub fn release(&self) {
        self.taken.store(false, Ordering::SeqCst);
        self.changed.send_replace(false);
    }

    /// Waits for an attached page to go away, if there is one.
    pub async fn idle(&self) {
        let mut changed = self.changed.subscribe();
        while *changed.borrow_and_update() {
            if changed.changed().await.is_err() {
                return;
            }
        }
    }

    /// Asks every attached page to leave and nothing else to arrive.
    pub fn stop(&self) {
        self.shutdown.send_replace(true);
    }

    /// Told when this process is leaving.
    pub fn shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }
}

/// Everything a connection reads.
pub struct State {
    /// The connection flags the client child is started with.
    pub client: client::Args,
    /// The one window.
    pub slot: Slot,
    hosts: Vec<String>,
    origins: Vec<String>,
}

impl State {
    pub fn new(parsed: Parsed, bind: SocketAddr) -> Self {
        let port = bind.port();
        let mut hosts = Vec::new();
        let mut origins = Vec::new();
        for name in ["127.0.0.1", "localhost", "[::1]"] {
            let host = format!("{name}:{port}");
            origins.push(format!("http://{host}"));
            hosts.push(host);
        }
        Self {
            client: parsed.client,
            slot: Slot::new(),
            hosts,
            origins,
        }
    }

    pub fn stop(&self) {
        self.slot.stop();
    }

    pub async fn idle(&self) {
        self.slot.idle().await;
    }

    pub fn shutdown(&self) -> watch::Receiver<bool> {
        self.slot.shutdown()
    }

    /// Whether a request comes from the page this process serves.
    ///
    /// # Invariant
    ///
    /// The Host must be a loopback name and, when a browser sends one, the Origin must
    /// be that same name: the page can drive the harness, so another site must not
    /// reach it through a rebound name. See `docs/notes/webui-server.md`.
    fn same_origin(&self, request: &Request<Incoming>) -> bool {
        let Some(host) = request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        if !self.hosts.iter().any(|allowed| allowed == host) {
            return false;
        }
        match request
            .headers()
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
        {
            None => true,
            Some(origin) => self.origins.iter().any(|allowed| allowed == origin),
        }
    }
}

/// Serves the page until the listener itself fails.
pub async fn serve(listener: TcpListener, state: Arc<State>) -> anyhow::Result<()> {
    loop {
        let (stream, _peer) = listener.accept().await.context("accepting a page")?;
        let state = state.clone();
        tokio::spawn(async move {
            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(move |request| answer(request, state.clone())),
                )
                .with_upgrades();
            if let Err(error) = connection.await {
                eprintln!("gongga-webui: a connection ended in error: {error}");
            }
        });
    }
}

async fn answer(
    request: Request<Incoming>,
    state: Arc<State>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() != Method::GET {
        return Ok(described(StatusCode::METHOD_NOT_ALLOWED, "only GET"));
    }
    let path = request.uri().path().to_string();
    match path.as_str() {
        "/ws" => Ok(upgrade(request, state)),
        "/favicon.ico" => Ok(described(StatusCode::NO_CONTENT, "")),
        _ => Ok(match page::asset(path.as_str()) {
            Some(asset) => body(StatusCode::OK, asset.kind, asset.body),
            None => described(StatusCode::NOT_FOUND, "no such file in the page"),
        }),
    }
}

/// Answers a WebSocket handshake and hands the socket to a window.
fn upgrade(request: Request<Incoming>, state: Arc<State>) -> Response<Full<Bytes>> {
    if !state.same_origin(&request) {
        return described(
            StatusCode::FORBIDDEN,
            "open the page on 127.0.0.1 or localhost",
        );
    }
    let Some(key) = request
        .headers()
        .get("sec-websocket-key")
        .and_then(|value| value.to_str().ok())
    else {
        return described(StatusCode::BAD_REQUEST, "not a websocket upgrade");
    };
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    let upgraded = hyper::upgrade::on(request);
    tokio::spawn(async move {
        match upgraded.await {
            Ok(upgraded) => window::attach(TokioIo::new(upgraded), state).await,
            Err(error) => {
                eprintln!("gongga-webui: a page's socket could not be taken over: {error}")
            }
        }
    });
    let built = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "upgrade")
        .header(UPGRADE, "websocket")
        .header("sec-websocket-accept", accept)
        .body(Full::default());
    match built {
        Ok(response) => response,
        Err(error) => {
            eprintln!("gongga-webui: could not build an upgrade: {error}");
            described(StatusCode::INTERNAL_SERVER_ERROR, "no upgrade")
        }
    }
}

/// One embedded file, never cached: a rebuilt page must be the page served.
fn body(status: StatusCode, kind: &str, text: &'static str) -> Response<Full<Bytes>> {
    let built = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, kind)
        .header(CACHE_CONTROL, "no-store")
        .body(Full::from(Bytes::from_static(text.as_bytes())));
    match built {
        Ok(response) => response,
        Err(_) => Response::new(Full::default()),
    }
}

fn described(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    body(status, "text/plain; charset=utf-8", text)
}
