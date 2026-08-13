use std::{
    net::SocketAddr,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use askama::Template;
use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use chrono::Local;
use tokio::sync::oneshot;

use crate::config::ExternalDockConfig;

const HTMX: &str = include_str!("../assets/web/htmx.min.js");
const HTMX_WS: &str = include_str!("../assets/web/htmx-ext-ws.js");

#[derive(Template)]
#[template(path = "external-dock.html")]
struct DockTemplate {
    initial_time: String,
}

pub struct WebServer {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl WebServer {
    pub fn start(config: &ExternalDockConfig) -> Result<Self> {
        let address = SocketAddr::new(config.bind_address, config.port);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("raylander-web".into())
            .spawn(move || run(address, shutdown_rx, ready_tx))
            .context("failed to start external dock server thread")?;

        ready_rx
            .recv()
            .context("external dock server stopped during startup")??;
        tracing::info!(%address, "external dock server listening");
        Ok(Self {
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(address: SocketAddr, shutdown: oneshot::Receiver<()>, ready: mpsc::SyncSender<Result<()>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error).context("failed to create web runtime"));
            return;
        }
    };
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(error) => {
                let _ = ready.send(Err(error).with_context(|| format!("failed to bind {address}")));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        if let Err(error) = axum::serve(listener, router())
            .with_graceful_shutdown(async {
                let _ = shutdown.await;
            })
            .await
        {
            tracing::warn!(%error, "external dock server stopped unexpectedly");
        }
    });
}

fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(websocket))
        .route("/assets/htmx.min.js", get(|| async { javascript(HTMX) }))
        .route(
            "/assets/htmx-ext-ws.js",
            get(|| async { javascript(HTMX_WS) }),
        )
}

async fn index() -> Response {
    match (DockTemplate {
        initial_time: server_time(),
    })
    .render()
    {
        Ok(page) => Html(page).into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to render external dock page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn websocket(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(send_time)
}

async fn send_time(mut socket: WebSocket) {
    loop {
        let fragment = format!(
            r#"<time id="server-time" hx-swap-oob="innerHTML">{}</time>"#,
            server_time()
        );
        if socket.send(Message::Text(fragment.into())).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn javascript(source: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        source,
    )
}

fn server_time() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_page_contains_htmx_websocket_and_greeting() {
        let page = DockTemplate {
            initial_time: "12:34:56".into(),
        }
        .render()
        .unwrap();
        assert!(page.contains("Hello World"));
        assert!(page.contains("hx-ext=\"ws\""));
        assert!(page.contains("ws-connect=\"/ws\""));
        assert!(page.contains("htmx:wsClose"));
        assert!(page.contains("setTimeout(reloadWhenReady, 1000)"));
        assert!(page.contains("12:34:56"));
    }

    #[test]
    fn bundled_htmx_assets_are_present() {
        assert!(HTMX.contains("htmx"));
        assert!(HTMX_WS.contains("webSocket"));
    }
}
