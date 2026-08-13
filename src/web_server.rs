use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    os::unix::net::UnixStream,
    process::{Command, Stdio},
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
use serde::Deserialize;
use tokio::sync::{broadcast, oneshot};

use crate::{
    audio::{AudioObserver, WpctlBackend},
    bluetooth::{BluetoothAvailability, BluetoothBackend},
    config::ExternalDockConfig,
    display::DdcutilBackend,
    herdr::{AgentStatus, HerdrBackend},
    input_method::{Fcitx5Backend, InputMethodStatus},
    media::{MediaAction, PlayerctlBackend},
    network::NmcliBackend,
    packages::PackageBackend,
    system_status::SystemStatusBackend,
};

const EXTERNAL_DOCK_CSS: &str = include_str!("../assets/web/external-dock.css");
const HTMX: &str = include_str!("../assets/web/htmx.min.js");
const HTMX_WS: &str = include_str!("../assets/web/htmx-ext-ws.js");
const ARCH_LOGO: &[u8] = include_bytes!("../assets/web/arch-logo.png");
const PI_LOGO: &str = include_str!("../assets/web/pi-logo.svg");
static WEATHER_COORDS: OnceLock<(String, String)> = OnceLock::new();
static EVENTS: OnceLock<broadcast::Sender<DockEvent>> = OnceLock::new();
static DOCK_DISPLAY: OnceLock<u32> = OnceLock::new();
static AGENT_STATE: OnceLock<Mutex<AgentWidgetState>> = OnceLock::new();

#[derive(Default)]
struct AgentWidgetState {
    agents: Vec<AgentStatus>,
    completed: Vec<(String, String)>,
    completed_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
enum DockEvent {
    Volume,
    Media,
    InputMethod,
    Hyprland,
    DisplayPower,
    Agents,
}

pub(crate) fn notify_display_power() {
    if let Some(events) = EVENTS.get() {
        let _ = events.send(DockEvent::DisplayPower);
    }
}

#[derive(Template)]
#[template(path = "external-dock.html")]
struct DockTemplate;

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
        let (events, _) = broadcast::channel(32);
        let _ = EVENTS.set(events.clone());
        start_event_sources(events);
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
        .route("/volume/{percent}", post(set_volume))
        .route("/brightness/{percent}", post(set_brightness))
        .route("/workspace/{id}", post(focus_workspace))
        .route("/weather-details", get(weather_details))
        .route(
            "/assets/external-dock.css",
            get(|| async { stylesheet(EXTERNAL_DOCK_CSS) }),
        )
        .route("/assets/htmx.min.js", get(|| async { javascript(HTMX) }))
        .route(
            "/assets/htmx-ext-ws.js",
            get(|| async { javascript(HTMX_WS) }),
        )
        .route("/assets/arch-logo.png", get(arch_logo))
        .route("/assets/pi-logo.svg", get(pi_logo))
}

async fn index() -> Response {
    match DockTemplate.render() {
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

async fn set_volume(axum::extract::Path(percent): axum::extract::Path<u8>) -> StatusCode {
    if percent > 100 {
        return StatusCode::BAD_REQUEST;
    }
    let result = tokio::task::spawn_blocking(move || {
        Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{percent}%")])
            .status()
    })
    .await;
    match result {
        Ok(Ok(status)) if status.success() => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn set_brightness(axum::extract::Path(percent): axum::extract::Path<u8>) -> StatusCode {
    if percent > 100 {
        return StatusCode::BAD_REQUEST;
    }
    let result = tokio::task::spawn_blocking(move || {
        let display = DOCK_DISPLAY.get().copied().or_else(|| {
            DdcutilBackend
                .displays()
                .ok()?
                .into_iter()
                .find(|display| {
                    display
                        .connector
                        .as_deref()
                        .is_some_and(|connector| connector.ends_with("DP-2"))
                })
                .map(|display| *DOCK_DISPLAY.get_or_init(|| display.number))
        });
        display.map(|display| DdcutilBackend.set_brightness(display, percent))
    })
    .await;
    match result {
        Ok(Some(Ok(()))) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn focus_workspace(axum::extract::Path(id): axum::extract::Path<i32>) -> StatusCode {
    if !(1..=27).contains(&id) {
        return StatusCode::BAD_REQUEST;
    }
    let result = tokio::task::spawn_blocking(move || {
        Command::new("hyprctl")
            .args(["dispatch", &format!("hl.dsp.focus({{workspace={id}}})")])
            .status()
    })
    .await;
    match result {
        Ok(Ok(status)) if status.success() => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[derive(Deserialize)]
struct WeatherDetailsQuery {
    city: Option<String>,
}

async fn weather_details(
    axum::extract::Query(query): axum::extract::Query<WeatherDetailsQuery>,
) -> Response {
    let configured = WEATHER_COORDS.get();
    let (latitude, longitude) = match query.city.as_deref().unwrap_or("minneapolis") {
        "minneapolis" => ("44.9778", "-93.2650"),
        "los-angeles" => ("34.0522", "-118.2437"),
        "detroit" => ("42.3314", "-83.0458"),
        "new-york" => ("40.7128", "-74.0060"),
        "guangzhou" => ("23.1291", "113.2644"),
        "configured" => match configured {
            Some((latitude, longitude)) => (latitude.as_str(), longitude.as_str()),
            None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m&hourly=temperature_2m,precipitation_probability,weather_code&daily=weather_code,temperature_2m_min,temperature_2m_max,precipitation_probability_max,sunrise,sunset&forecast_days=7&timezone=auto"
    );
    match tokio::task::spawn_blocking(move || {
        Command::new("curl")
            .args(["--fail", "--silent", "--max-time", "8", &url])
            .output()
    })
    .await
    {
        Ok(Ok(output)) if output.status.success() => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            output.stdout,
        )
            .into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
    let Some(events) = EVENTS.get() else { return };
    let mut events = events.subscribe();
    let initial = tokio::task::spawn_blocking(dashboard_fragment)
        .await
        .unwrap_or_default();
    if socket.send(Message::Text(initial.into())).await.is_err() {
        return;
    }
    loop {
        let Ok(event) = events.recv().await else {
            break;
        };
        let fragment = tokio::task::spawn_blocking(move || event_fragment(event))
            .await
            .unwrap_or_default();
        if socket.send(Message::Text(fragment.into())).await.is_err() {
            break;
        }
    }
}

fn start_event_sources(events: broadcast::Sender<DockEvent>) {
    let (observer, audio_events) = AudioObserver::start();
    thread::spawn(move || {
        let _observer = observer;
        while audio_events.recv().is_ok() {
            let _ = events.send(DockEvent::Volume);
        }
    });
    observe_command(
        DockEvent::Media,
        [
            "playerctl",
            "--all-players",
            "metadata",
            "--follow",
            "--format",
            "{{status}} {{xesam:title}}",
        ],
    );
    observe_input_method();
    observe_hyprland();
    observe_agents();
}

fn observe_command<const N: usize>(event: DockEvent, command: [&'static str; N]) {
    thread::spawn(move || {
        loop {
            let Some(events) = EVENTS.get() else { return };
            let Ok(mut child) = Command::new(command[0])
                .args(&command[1..])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                thread::sleep(Duration::from_secs(5));
                continue;
            };
            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines() {
                    if line.is_err() {
                        break;
                    }
                    let _ = events.send(event);
                }
            }
            let _ = child.wait();
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn observe_input_method() {
    thread::spawn(|| {
        let mut previous = None;
        loop {
            let current = Fcitx5Backend.current().ok();
            if previous.as_ref() != current.as_ref() {
                previous = current;
                if let Some(events) = EVENTS.get() {
                    let _ = events.send(DockEvent::InputMethod);
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

fn observe_agents() {
    thread::spawn(|| {
        let mut previous = Vec::<AgentStatus>::new();
        loop {
            if let Ok(current) = HerdrBackend.agents()
                && current != previous
            {
                let completed = current
                    .iter()
                    .filter(|agent| {
                        agent.status == "done"
                            || (agent.status == "idle"
                                && previous.iter().any(|old| {
                                    old.workspace == agent.workspace
                                        && old.pane == agent.pane
                                        && old.status == "working"
                                }))
                    })
                    .map(|agent| (agent.workspace.clone(), agent.pane.clone()))
                    .collect::<Vec<_>>();
                let state = AGENT_STATE.get_or_init(|| Mutex::new(AgentWidgetState::default()));
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.agents.clone_from(&current);
                if !completed.is_empty() {
                    for agent in completed {
                        if !state.completed.contains(&agent) {
                            state.completed.push(agent);
                        }
                    }
                    state.completed_at = Some(Instant::now());
                }
                previous = current;
                drop(state);
                if let Some(events) = EVENTS.get() {
                    let _ = events.send(DockEvent::Agents);
                }
            }
            if let Some(state) = AGENT_STATE.get() {
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                if state
                    .completed_at
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(10))
                {
                    state.completed.clear();
                    state.completed_at = None;
                    drop(state);
                    if let Some(events) = EVENTS.get() {
                        let _ = events.send(DockEvent::Agents);
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

fn observe_hyprland() {
    thread::spawn(|| {
        loop {
            let Some(signature) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") else {
                thread::sleep(Duration::from_secs(5));
                continue;
            };
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
            let path = std::path::PathBuf::from(runtime)
                .join("hypr")
                .join(signature)
                .join(".socket2.sock");
            let Ok(stream) = UnixStream::connect(path) else {
                thread::sleep(Duration::from_secs(2));
                continue;
            };
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if let Some(events) = EVENTS.get() {
                    if line.starts_with("activewindow") || line.starts_with("workspace") {
                        let _ = events.send(DockEvent::Hyprland);
                    } else if line.starts_with("monitoradded")
                        || line.starts_with("monitorremoved")
                        || line.starts_with("configreloaded")
                    {
                        let _ = events.send(DockEvent::DisplayPower);
                    }
                }
            }
        }
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprMonitor {
    id: i32,
    focused: bool,
    dpms_status: bool,
    active_workspace: HyprWorkspaceRef,
}
#[derive(Deserialize)]
struct HyprWorkspaceRef {
    id: i32,
}
#[derive(Deserialize)]
struct HyprWorkspace {
    id: i32,
    windows: u32,
    #[serde(rename = "lastwindowtitle")]
    last_window_title: String,
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

fn event_fragment(event: DockEvent) -> String {
    match event {
        DockEvent::Volume => volume_fragment(),
        DockEvent::Media => media_fragment(),
        DockEvent::InputMethod => input_method_fragment(),
        DockEvent::Hyprland => hyprland_oob_fragment(),
        DockEvent::DisplayPower => display_power_fragment(),
        DockEvent::Agents => agents_fragment(),
    }
}

fn volume_fragment() -> String {
    let Ok(volume) = WpctlBackend.current() else {
        return String::new();
    };
    let percent = if volume.muted {
        0
    } else {
        volume.percent.min(100)
    };
    let mut html = String::new();
    flash_value(&mut html, "volume", &format!("{percent}%"));
    html.push_str(&format!(r#"<div id="volume-fill" class="ios-fill bg-[#f1f2f5]" hx-swap-oob="outerHTML" style="height:{percent}%"></div>"#));
    html
}

fn input_method_fragment() -> String {
    let input = Fcitx5Backend.current().ok();
    let class = match input.as_ref().map(|state| &state.status) {
        Some(InputMethodStatus::Active) => "control-icon active font-mono text-[10px]",
        Some(InputMethodStatus::Inactive) => "control-icon font-mono text-[10px]",
        _ => "control-icon warning font-mono text-[10px]",
    };
    let label = input
        .and_then(|state| state.name)
        .map(|name| if name.contains("pinyin") { "中" } else { "A" })
        .unwrap_or("?");
    let mut html = String::new();
    class_replace(&mut html, "input-control", class, label);
    html
}

fn display_power_fragment() -> String {
    let enabled = hyprctl_json::<Vec<HyprMonitor>>("monitors")
        .is_some_and(|monitors| monitors.iter().any(|monitor| monitor.dpms_status));
    format!(r#"<div id="display-power" data-on="{enabled}" hidden hx-swap-oob="outerHTML"></div>"#)
}

fn hyprland_oob_fragment() -> String {
    let mut html = String::new();
    if let Some(monitors) = hyprland_fragment() {
        raw_replace(&mut html, "monitors", &monitors);
    }
    html
}

fn media_fragment() -> String {
    let mut html = String::new();
    match PlayerctlBackend.current() {
        Ok(Some(media)) => {
            html.push_str(&format!(r#"<div id="media-empty" class="media-empty hidden" hx-swap-oob="outerHTML"></div><div id="media-copy" class="media-copy" hx-swap-oob="outerHTML"><p id="media-title" class="value truncate">{}</p><p id="media-artist" class="truncate text-[9px] text-[#a6acb9]">{}</p><div class="media-controls"><button onclick="fetch('/media/previous',{{method:'POST'}})" aria-label="Previous"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button><button onclick="fetch('/media/play-pause',{{method:'POST'}})" aria-label="Play or pause"><svg viewBox="0 0 24 24"><path d="m8 5 11 7-11 7z"/></svg></button><button onclick="fetch('/media/next',{{method:'POST'}})" aria-label="Next"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></div>"#, escape(&media.title), escape(&media.artists.join(", "))));
            if media.artwork_url.is_some() { html.push_str(&format!(r#"<img id="media-art" src="/artwork?t={}" alt="" hx-swap-oob="outerHTML" onerror="this.hidden=true" onload="this.hidden=false">"#, Instant::now().elapsed().as_nanos())); }
        }
        _ => html.push_str(r#"<img id="media-art" hidden hx-swap-oob="outerHTML" alt=""><div id="media-empty" class="media-empty" hx-swap-oob="outerHTML" aria-hidden="true"><img src="/assets/arch-logo.png" alt=""></div><div id="media-copy" class="media-copy hidden" hx-swap-oob="outerHTML"></div>"#),
    }
    html
}

fn agents_fragment() -> String {
    let state = AGENT_STATE.get_or_init(|| Mutex::new(AgentWidgetState::default()));
    let state = state.lock().unwrap_or_else(|error| error.into_inner());
    let blocked = state
        .agents
        .iter()
        .filter(|agent| agent.status == "blocked")
        .collect::<Vec<_>>();
    let working = state
        .agents
        .iter()
        .filter(|agent| agent.status == "working")
        .collect::<Vec<_>>();
    let mut completed_agents = state.completed.clone();
    for agent in state.agents.iter().filter(|agent| agent.status == "done") {
        let item = (agent.workspace.clone(), agent.pane.clone());
        if !completed_agents.contains(&item) {
            completed_agents.push(item);
        }
    }
    let completed = state.agents.iter().any(|agent| agent.status == "done")
        || state
            .completed_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(10));
    let (class, title, agents): (&str, &str, Vec<(&str, &str, &str)>) = if !blocked.is_empty() {
        (
            "card agent-card attention",
            "Attention needed",
            blocked
                .iter()
                .map(|a| (a.workspace.as_str(), a.pane.as_str(), "blocked"))
                .collect(),
        )
    } else if completed {
        (
            "card agent-card completed",
            "Agent completed",
            completed_agents
                .iter()
                .map(|(w, p)| (w.as_str(), p.as_str(), "done"))
                .collect(),
        )
    } else if !working.is_empty() {
        (
            "card agent-card running",
            "Agents running",
            working
                .iter()
                .map(|a| (a.workspace.as_str(), a.pane.as_str(), "working"))
                .collect(),
        )
    } else {
        ("card agent-card idle", "Agents idle", Vec::new())
    };
    let rows = agents
        .into_iter()
        .map(|(workspace, pane, status)| {
            let indicator = if status == "working" {
                r#"<svg class="agent-working-icon" viewBox="0 0 24 18" role="img" aria-label="working"><rect x="2" y="7" width="4" height="9" rx="2"/><rect x="10" y="2" width="4" height="14" rx="2"/><rect x="18" y="5" width="4" height="11" rx="2"/></svg>"#
                    .to_owned()
            } else {
                format!(r#"<b class="agent-status {status}" aria-label="{status}"></b>"#)
            };
            format!(
                r#"<div class="agent-row"><span>{} <i>·</i> {}</span>{indicator}</div>"#,
                escape(workspace),
                escape(pane)
            )
        })
        .collect::<String>();
    let idle_logo = if rows.is_empty() && title == "Agents idle" {
        r#"<div class="agent-idle"><img src="/assets/pi-logo.svg" alt="Pi"></div>"#
    } else {
        ""
    };
    format!(
        r#"<section id="agent-card" class="{class}" hx-swap-oob="outerHTML"><div class="agent-list">{rows}{idle_logo}</div></section>"#
    )
}

fn dashboard_fragment() -> String {
    static BLUETOOTH: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static PACKAGES: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static BRIGHTNESS: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static SYSTEM: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static WEATHER: OnceLock<Mutex<FragmentCache>> = OnceLock::new();
    static LAST_VOLUME: OnceLock<Mutex<Option<u16>>> = OnceLock::new();
    static LAST_BRIGHTNESS: OnceLock<Mutex<Option<u8>>> = OnceLock::new();

    let mut html = display_power_fragment();
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
        Duration::from_millis(400),
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
    match PlayerctlBackend.current() {
        Ok(Some(media)) => {
            replace(&mut html, "media-title", &media.title);
            replace(&mut html, "media-artist", &media.artists.join(", "));
            html.push_str(r#"<div id="media-empty" class="media-empty hidden" hx-swap-oob="outerHTML"></div><div id="media-copy" class="media-copy" hx-swap-oob="outerHTML"><p id="media-title" class="value truncate"></p><p id="media-artist" class="truncate text-[9px] text-[#a6acb9]"></p><div class="media-controls"><button onclick="fetch('/media/previous',{method:'POST'})" aria-label="Previous"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button><button onclick="fetch('/media/play-pause',{method:'POST'})" aria-label="Play or pause"><svg viewBox="0 0 24 24"><path d="m8 5 11 7-11 7z"/></svg></button><button onclick="fetch('/media/next',{method:'POST'})" aria-label="Next"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></div>"#);
            if media.artwork_url.is_some() {
                html.push_str(r#"<img id="media-art" src="/artwork" alt="" hx-swap-oob="outerHTML" onerror="this.hidden=true" onload="this.hidden=false">"#);
            }
        }
        _ => {
            html.push_str(r#"<img id="media-art" hidden hx-swap-oob="outerHTML" alt=""><div id="media-empty" class="media-empty" hx-swap-oob="outerHTML" aria-hidden="true"><img src="/assets/arch-logo.png" alt=""></div><div id="media-copy" class="media-copy hidden" hx-swap-oob="outerHTML"></div>"#);
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
    if let Some(monitors) = hyprland_fragment() {
        raw_replace(&mut html, "monitors", &monitors);
    }
    html.push_str(&cached_fragment(&BRIGHTNESS, Duration::from_secs(5 * 60), || {
        let displays = DdcutilBackend.all_brightness().ok()?;
        let (display, value) = displays.into_iter().find(|(display, _)| {
            display
                .connector
                .as_deref()
                .is_some_and(|connector| connector.ends_with("DP-2"))
        })?;
        let _ = DOCK_DISPLAY.set(display.number);
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
    html.push_str(&agents_fragment());
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

fn hyprland_fragment() -> Option<String> {
    let monitors: Vec<HyprMonitor> = hyprctl_json("monitors")?;
    let workspaces: Vec<HyprWorkspace> = hyprctl_json("workspaces")?;
    let focused_title = monitors
        .iter()
        .find(|monitor| monitor.focused)
        .and_then(|monitor| {
            workspaces
                .iter()
                .find(|workspace| workspace.id == monitor.active_workspace.id)
        })
        .map(|workspace| workspace.last_window_title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Desktop");
    let focused_title = if focused_title.to_ascii_lowercase().contains("firefox") {
        "Firefox"
    } else {
        focused_title
    };
    let monitor_views = monitors
        .into_iter()
        .map(|monitor| {
            let base = monitor.id * 9 + 1;
            let active = monitor.active_workspace.id;
            let dots = [6, 7, 8, 3, 4, 5, 0, 1, 2]
                .into_iter()
                .map(|offset| {
                    let id = base + offset;
                    let occupied = workspaces
                        .iter()
                        .any(|workspace| workspace.id == id && workspace.windows > 0);
                    let class = if id == active && monitor.focused {
                        "active focused"
                    } else if id == active {
                        "active"
                    } else if occupied {
                        "occupied"
                    } else {
                        ""
                    };
                    format!(r#"<i class="{class}"></i>"#)
                })
                .collect::<String>();
            format!(
                r#"<div class="monitor-view" data-base="{base}" data-active="{active}"><div class="workspace-dots">{dots}</div></div>"#
            )
        })
        .collect::<String>();
    Some(format!(
        r#"<p class="focus-title" title="{}">{}</p><div class="monitor-grid">{monitor_views}</div>"#,
        escape(focused_title),
        escape(focused_title)
    ))
}

fn hyprctl_json<T: for<'de> Deserialize<'de>>(query: &str) -> Option<T> {
    let output = Command::new("hyprctl").args(["-j", query]).output().ok()?;
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

async fn pi_logo() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], PI_LOGO)
}

async fn arch_logo() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], ARCH_LOGO)
}

fn javascript(source: &'static str) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        source,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_page_contains_htmx_websocket_and_greeting() {
        let page = DockTemplate.render().unwrap();
        assert!(page.contains("id=\"local-time\""));
        assert!(page.contains("id=\"system-card\""));
        assert!(page.contains("id=\"weather-card\""));
        assert!(page.contains("/assets/external-dock.css"));
        assert!(page.contains("class=\"min-h-screen"));
        assert!(!page.contains("<style>"));
        assert!(page.contains("hx-ext=\"ws\""));
        assert!(page.contains("ws-connect=\"/ws\""));
        assert!(page.contains("htmx:wsClose"));
        assert!(page.contains("setTimeout(retry,1000)"));
        assert!(page.contains("toLocaleTimeString"));
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
                    let _ = hyprland_fragment();
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
