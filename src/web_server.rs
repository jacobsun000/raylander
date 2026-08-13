use std::{
    net::SocketAddr,
    process::Command,
    sync::{Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
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
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::{
    audio::WpctlBackend,
    bluetooth::{BluetoothAvailability, BluetoothBackend},
    config::ExternalDockConfig,
    display::DdcutilBackend,
    media::PlayerctlBackend,
    packages::PackageBackend,
    system_status::SystemStatusBackend,
};

const EXTERNAL_DOCK_CSS: &str = include_str!("../assets/web/external-dock.css");
const HTMX: &str = include_str!("../assets/web/htmx.min.js");
const HTMX_WS: &str = include_str!("../assets/web/htmx-ext-ws.js");

#[derive(Template)]
#[template(path = "external-dock.html")]
struct DockTemplate {
    initial_date: String,
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
        .route(
            "/assets/external-dock.css",
            get(|| async { stylesheet(EXTERNAL_DOCK_CSS) }),
        )
        .route("/assets/htmx.min.js", get(|| async { javascript(HTMX) }))
        .route(
            "/assets/htmx-ext-ws.js",
            get(|| async { javascript(HTMX_WS) }),
        )
}

async fn index() -> Response {
    match (DockTemplate {
        initial_date: server_date(),
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
    if socket
        .send(Message::Text(clock_fragment().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        let fragment = tokio::task::spawn_blocking(dashboard_fragment)
            .await
            .unwrap_or_else(|_| clock_fragment());
        if socket.send(Message::Text(fragment.into())).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[derive(Deserialize)]
struct ActiveWindow {
    title: String,
    class: String,
    workspace: Workspace,
}
#[derive(Deserialize)]
struct Workspace {
    name: String,
    id: i32,
}

struct FragmentCache {
    updated_at: Option<Instant>,
    value: String,
}

fn cached_fragment(
    cache: &'static OnceLock<Mutex<FragmentCache>>,
    max_age: Duration,
    query: impl FnOnce() -> Option<String>,
) -> String {
    let cache = cache.get_or_init(|| {
        Mutex::new(FragmentCache {
            updated_at: None,
            value: String::new(),
        })
    });
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    if cache
        .updated_at
        .is_none_or(|updated| updated.elapsed() >= max_age)
        && let Some(value) = query()
    {
        cache.value = value;
        cache.updated_at = Some(Instant::now());
    }
    cache.value.clone()
}

fn dashboard_fragment() -> String {
    static BLUETOOTH: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static PACKAGES: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static BRIGHTNESS: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static SYSTEM: OnceLock<Mutex<FragmentCache>> = OnceLock::new();

    let mut html = clock_fragment();
    if let Ok(volume) = WpctlBackend.current() {
        replace(
            &mut html,
            "volume",
            &if volume.muted {
                "Muted".into()
            } else {
                format!("{}%", volume.percent)
            },
        );
    }
    html.push_str(&cached_fragment(
        &BLUETOOTH,
        Duration::from_secs(30),
        || {
            let state = zbus::block_on(BluetoothBackend.state()).ok()?;
            let value = match state.availability {
                BluetoothAvailability::Unavailable { .. } => "Unavailable".into(),
                BluetoothAvailability::Available => {
                    let connected: Vec<_> = state
                        .devices
                        .iter()
                        .filter(|d| d.connected)
                        .map(|d| d.name.as_str())
                        .collect();
                    if !connected.is_empty() {
                        connected.join(", ")
                    } else if state.adapters.iter().any(|a| a.powered) {
                        "On".into()
                    } else {
                        "Off".into()
                    }
                }
            };
            let mut fragment = String::new();
            replace(&mut fragment, "bluetooth", &value);
            Some(fragment)
        },
    ));
    if let Ok(Some(media)) = PlayerctlBackend.current() {
        replace(&mut html, "media-title", &media.title);
        replace(&mut html, "media-artist", &media.artists.join(", "));
    }
    html.push_str(&cached_fragment(
        &PACKAGES,
        Duration::from_secs(30 * 60),
        || {
            let packages = PackageBackend.current().ok()?;
            let mut fragment = String::new();
            replace(
                &mut fragment,
                "packages-total",
                &packages.total_unique().to_string(),
            );
            replace(
                &mut fragment,
                "packages-pacman",
                &format!("pacman {}", packages.official.len()),
            );
            replace(
                &mut fragment,
                "packages-paru",
                &format!("paru {}", packages.aur.len()),
            );
            Some(fragment)
        },
    ));
    if let Some(window) = active_window() {
        replace(
            &mut html,
            "window",
            &if window.title.trim().is_empty() {
                window.class
            } else {
                window.title
            },
        );
        let workspace = if window.workspace.name.is_empty() {
            window.workspace.id.to_string()
        } else {
            window.workspace.name
        };
        replace(&mut html, "workspace", &format!("Workspace {workspace}"));
    }
    html.push_str(&cached_fragment(&BRIGHTNESS, Duration::from_secs(5 * 60), || {
        let displays = DdcutilBackend.all_brightness().ok()?;
        let rows = displays.into_iter().map(|(display, value)| {
            let name = display.model.or(display.connector).unwrap_or_else(|| format!("Display {}", display.number));
            let percent = value.percent();
            format!(r#"<div><div class="mb-2 flex justify-between text-xs"><span class="truncate text-[#a6acb9]">{}</span><span class="font-mono tabular-nums">{}%</span></div><div class="h-1.5 overflow-hidden rounded-full bg-white/[.06]"><i class="block h-full rounded-full bg-[#e8a13a]" style="width:{}%"></i></div></div>"#, escape(&name), percent, percent)
        }).collect::<String>();
        let mut fragment = String::new();
        raw_replace(
            &mut fragment,
            "brightness",
            if rows.is_empty() {
                "<p>No DDC displays</p>"
            } else {
                &rows
            },
        );
        Some(fragment)
    }));
    html.push_str(&cached_fragment(&SYSTEM, Duration::from_secs(3), || {
        let status = SystemStatusBackend.current().ok()?;
        let mut fragment = String::new();
        metric(&mut fragment, "cpu", status.cpu_percent);
        metric(&mut fragment, "memory", status.memory_percent);
        metric(&mut fragment, "disk", status.disk_percent);
        if let Some(gpu) = status.gpu_percent {
            metric(&mut fragment, "gpu", gpu);
        }
        Some(fragment)
    }));
    html
}

fn active_window() -> Option<ActiveWindow> {
    let output = Command::new("hyprctl")
        .args(["-j", "activewindow"])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    serde_json::from_slice(&output.stdout).ok()
}

fn metric(html: &mut String, id: &str, percent: u8) {
    replace(html, id, &format!("{percent}%"));
    html.push_str(&format!(
        r#"<i id="{id}-bar" hx-swap-oob="outerHTML" style="width:{percent}%"></i>"#
    ));
}
fn replace(html: &mut String, id: &str, value: &str) {
    raw_replace(html, id, &escape(value));
}
fn raw_replace(html: &mut String, id: &str, value: &str) {
    html.push_str(&format!(
        r#"<span id="{id}" hx-swap-oob="innerHTML">{value}</span>"#
    ));
}
fn clock_fragment() -> String {
    let mut html = String::new();
    replace(&mut html, "date", &server_date());
    replace(&mut html, "server-time", &server_time());
    html
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn stylesheet(source: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], source)
}

fn javascript(source: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        source,
    )
}

fn server_date() -> String {
    Local::now().format("%b %-d, %a").to_string()
}

fn server_time() -> String {
    Local::now().format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_page_contains_htmx_websocket_and_greeting() {
        let page = DockTemplate {
            initial_date: "Aug 14, Wed".into(),
            initial_time: "12:34".into(),
        }
        .render()
        .unwrap();
        assert!(page.contains("Aug 14, Wed"));
        assert!(page.contains("System status"));
        assert!(page.contains("/assets/external-dock.css"));
        assert!(page.contains("class=\"min-h-screen"));
        assert!(!page.contains("<style>"));
        assert!(page.contains("hx-ext=\"ws\""));
        assert!(page.contains("ws-connect=\"/ws\""));
        assert!(page.contains("htmx:wsClose"));
        assert!(page.contains("setTimeout(reloadWhenReady, 1000)"));
        assert!(page.contains("12:34"));
    }

    #[test]
    fn bundled_web_assets_are_present() {
        assert!(EXTERNAL_DOCK_CSS.contains(".min-h-screen"));
        assert!(HTMX.contains("htmx"));
        assert!(HTMX_WS.contains("webSocket"));
    }
}
