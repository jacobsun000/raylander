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
    routing::{get, post},
};
use chrono::Local;
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::{
    audio::WpctlBackend,
    bluetooth::{BluetoothAvailability, BluetoothBackend},
    config::ExternalDockConfig,
    display::DdcutilBackend,
    input_method::{Fcitx5Backend, InputMethodStatus},
    media::{MediaAction, PlayerctlBackend},
    network::NmcliBackend,
    packages::PackageBackend,
    system_status::SystemStatusBackend,
};

const EXTERNAL_DOCK_CSS: &str = include_str!("../assets/web/external-dock.css");
const HTMX: &str = include_str!("../assets/web/htmx.min.js");
const HTMX_WS: &str = include_str!("../assets/web/htmx-ext-ws.js");
static WEATHER_COORDS: OnceLock<(String, String)> = OnceLock::new();

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
        let _ = WEATHER_COORDS.set((
            config.weather_latitude.clone(),
            config.weather_longitude.clone(),
        ));
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
        .route("/artwork", get(artwork))
        .route("/media/{action}", post(media_action))
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

async fn media_action(axum::extract::Path(action): axum::extract::Path<String>) -> StatusCode {
    let action = match action.as_str() {
        "previous" => MediaAction::Previous,
        "play-pause" => MediaAction::PlayPause,
        "next" => MediaAction::Next,
        _ => return StatusCode::NOT_FOUND,
    };
    match tokio::task::spawn_blocking(move || PlayerctlBackend.apply(None, action)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn artwork() -> Response {
    let result = tokio::task::spawn_blocking(|| {
        let media = PlayerctlBackend.current().ok()??;
        let url = media.artwork_url?;
        let path = url.strip_prefix("file://")?;
        std::fs::read(path).ok()
    })
    .await
    .ok()
    .flatten();
    match result {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/*"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
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
    static WEATHER: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static LAST_VOLUME: OnceLock<Mutex<Option<u16>>> = OnceLock::new();
    static LAST_BRIGHTNESS: OnceLock<Mutex<Option<u8>>> = OnceLock::new();

    let mut html = clock_fragment();
    if let Ok(volume) = WpctlBackend.current() {
        let percent = if volume.muted {
            0
        } else {
            volume.percent.min(100)
        };
        if value_changed(&LAST_VOLUME, percent) {
            flash_value(&mut html, "volume", &format!("{percent}%"));
        }
        html.push_str(&format!(
            r#"<div id="volume-fill" class="ios-fill bg-[#f1f2f5]" hx-swap-oob="outerHTML" style="height:{percent}%"></div>"#
        ));
    }
    html.push_str(&cached_fragment(
        &BLUETOOTH,
        Duration::from_secs(30),
        || {
            let state = zbus::block_on(BluetoothBackend.state()).ok()?;
            let bluetooth_class = match state.availability {
                BluetoothAvailability::Unavailable { .. } => "control-icon",
                BluetoothAvailability::Available if state.devices.iter().any(|d| d.connected) => {
                    "control-icon connected"
                }
                BluetoothAvailability::Available if state.adapters.iter().any(|a| a.powered) => {
                    "control-icon active"
                }
                BluetoothAvailability::Available => "control-icon",
            };
            let network = NmcliBackend.current().ok();
            let network_class = match network {
                Some(ref state) if !state.connections.is_empty() => "control-icon connected",
                Some(ref state) if state.wifi_enabled => "control-icon active",
                _ => "control-icon",
            };
            let input = Fcitx5Backend.current().ok();
            let input_class = match input.as_ref().map(|state| &state.status) {
                Some(InputMethodStatus::Active) => "control-icon active font-mono text-[10px]",
                Some(InputMethodStatus::Inactive) => "control-icon font-mono text-[10px]",
                _ => "control-icon warning font-mono text-[10px]",
            };
            let input_label = input.and_then(|state| state.name).map(|name| {
                if name.contains("pinyin") { "中" } else { "A" }
            }).unwrap_or("?");
            let mut fragment = String::new();
            class_replace(&mut fragment, "bluetooth-control", bluetooth_class, "ᛒ");
            class_replace(&mut fragment, "network-control", network_class, r#"<svg viewBox="0 0 24 24"><path d="M2 8.8a16 16 0 0 1 20 0l-2 2.4a12.8 12.8 0 0 0-16 0L2 8.8Zm4 4.8a9.6 9.6 0 0 1 12 0L16 16a6.4 6.4 0 0 0-8 0l-2-2.4ZM9.8 18.4a3.2 3.2 0 0 1 4.4 0L12 21l-2.2-2.6Z"/></svg>"#);
            class_replace(&mut fragment, "input-control", input_class, input_label);
            Some(fragment)
        },
    ));
    if let Ok(Some(media)) = PlayerctlBackend.current() {
        replace(&mut html, "media-title", &media.title);
        replace(&mut html, "media-artist", &media.artists.join(", "));
        if media.artwork_url.is_some() {
            html.push_str(r#"<img id="media-art" src="/artwork" alt="" hx-swap-oob="outerHTML" onerror="this.hidden=true" onload="this.hidden=false">"#);
        }
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
                &format!("◆ {}", packages.official.len()),
            );
            replace(
                &mut fragment,
                "packages-paru",
                &format!("▲ {}", packages.aur.len()),
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
        let (_, value) = displays.into_iter().find(|(display, _)| {
            display
                .connector
                .as_deref()
                .is_some_and(|connector| connector.ends_with("DP-2"))
        })?;
        let percent = value.percent();
        let mut fragment = String::new();
        if value_changed(&LAST_BRIGHTNESS, percent) {
            flash_value(&mut fragment, "brightness", &format!("{percent}%"));
        }
        fragment.push_str(&format!(
            r#"<div id="brightness-fill" class="ios-fill bg-[#f1f2f5]" hx-swap-oob="outerHTML" style="height:{percent}%"></div>"#
        ));
        Some(fragment)
    }));
    html.push_str(&cached_fragment(
        &WEATHER,
        Duration::from_secs(15 * 60),
        weather_fragment,
    ));
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

fn value_changed<T: Copy + PartialEq>(
    cache: &'static OnceLock<Mutex<Option<T>>>,
    value: T,
) -> bool {
    let mut previous = cache
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let changed = previous.is_some_and(|old| old != value);
    *previous = Some(value);
    changed
}

fn flash_value(html: &mut String, id: &str, value: &str) {
    html.push_str(&format!(
        r#"<span id="{id}" class="gauge-percent flash" hx-swap-oob="outerHTML">{}</span>"#,
        escape(value)
    ));
}

#[derive(Deserialize)]
struct WeatherResponse {
    current: CurrentWeather,
    daily: DailyWeather,
}
#[derive(Deserialize)]
struct CurrentWeather {
    temperature_2m: f32,
    weather_code: u8,
}
#[derive(Deserialize)]
struct DailyWeather {
    temperature_2m_min: Vec<f32>,
    temperature_2m_max: Vec<f32>,
}

fn weather_fragment() -> Option<String> {
    let (latitude, longitude) = WEATHER_COORDS.get()?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&current=temperature_2m,weather_code&daily=temperature_2m_min,temperature_2m_max&forecast_days=1&timezone=auto"
    );
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--max-time", "5", &url])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    let weather: WeatherResponse = serde_json::from_slice(&output.stdout).ok()?;
    let (icon, label) = weather_description(weather.current.weather_code);
    let mut html = String::new();
    replace(&mut html, "weather-icon", icon);
    let low = weather.daily.temperature_2m_min.first()?;
    let high = weather.daily.temperature_2m_max.first()?;
    replace(
        &mut html,
        "weather-temp",
        &format!("{:.0}°C", weather.current.temperature_2m),
    );
    replace(&mut html, "weather-low", &format!("{:.0}°", low));
    replace(&mut html, "weather-high", &format!("{:.0}°", high));
    replace(&mut html, "weather-label", label);
    Some(html)
}

fn weather_description(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("☀", "Clear"),
        1..=3 => ("◑", "Cloudy"),
        45 | 48 => ("≋", "Fog"),
        51..=67 | 80..=82 => ("☂", "Rain"),
        71..=77 | 85 | 86 => ("❄", "Snow"),
        95..=99 => ("ϟ", "Storm"),
        _ => ("·", "Weather"),
    }
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
fn class_replace(html: &mut String, id: &str, class: &str, value: &str) {
    html.push_str(&format!(
        r#"<span id="{id}" class="{class}" hx-swap-oob="outerHTML">{value}</span>"#
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
        assert!(page.contains("id=\"system-card\""));
        assert!(page.contains("id=\"weather-card\""));
        assert!(page.contains("/assets/external-dock.css"));
        assert!(page.contains("class=\"min-h-screen"));
        assert!(!page.contains("<style>"));
        assert!(page.contains("hx-ext=\"ws\""));
        assert!(page.contains("ws-connect=\"/ws\""));
        assert!(page.contains("htmx:wsClose"));
        assert!(page.contains("setTimeout(retry,1000)"));
        assert!(page.contains("12:34"));
    }

    /// Run with `cargo test component_load_benchmark -- --ignored --nocapture`.
    #[test]
    #[ignore = "queries live desktop services and the network"]
    fn component_load_benchmark() {
        let _ = WEATHER_COORDS.set(("30.2672".into(), "-97.7431".into()));
        type BenchmarkCase = (&'static str, Box<dyn Fn()>);
        let cases: [BenchmarkCase; 9] = [
            (
                "volume",
                Box::new(|| {
                    let _ = WpctlBackend.current();
                }),
            ),
            (
                "bluetooth",
                Box::new(|| {
                    let _ = zbus::block_on(BluetoothBackend.state());
                }),
            ),
            (
                "media",
                Box::new(|| {
                    let _ = PlayerctlBackend.current();
                }),
            ),
            (
                "packages",
                Box::new(|| {
                    let _ = PackageBackend.current();
                }),
            ),
            (
                "window",
                Box::new(|| {
                    let _ = active_window();
                }),
            ),
            (
                "brightness",
                Box::new(|| {
                    let _ = DdcutilBackend.all_brightness();
                }),
            ),
            (
                "system",
                Box::new(|| {
                    let _ = SystemStatusBackend.current();
                }),
            ),
            (
                "weather",
                Box::new(|| {
                    let _ = weather_fragment();
                }),
            ),
            (
                "full dashboard",
                Box::new(|| {
                    let _ = dashboard_fragment();
                }),
            ),
        ];
        println!("\ncomponent load benchmark");
        for (name, query) in cases {
            let started = Instant::now();
            query();
            println!(
                "{name:16} {:>8.2} ms",
                started.elapsed().as_secs_f64() * 1_000.0
            );
        }
    }

    #[test]
    fn bundled_web_assets_are_present() {
        assert!(EXTERNAL_DOCK_CSS.contains(".min-h-screen"));
        assert!(HTMX.contains("htmx"));
        assert!(HTMX_WS.contains("webSocket"));
    }
}
