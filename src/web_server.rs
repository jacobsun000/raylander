use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    io::{BufRead, BufReader},
    net::SocketAddr,
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
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
use chrono::{Datelike, Local, Timelike};
use serde::Deserialize;
use tokio::sync::{broadcast, oneshot};

use crate::{
    audio::WpctlBackend,
    audio_visualizer,
    bluetooth::{BluetoothAvailability, BluetoothBackend, BluetoothDevice, BluetoothState},
    calendar_events::{CalendarEvent, GoogleCalendarBackend},
    config::ExternalDockConfig,
    display::DdcutilBackend,
    herdr::{AgentStatus, HerdrBackend},
    input_method::{Fcitx5Backend, InputMethodAction, InputMethodStatus},
    media::{MediaAction, PlaybackStatus, PlayerctlBackend},
    network::NmcliBackend,
    packages::PackageBackend,
    system_status::SystemStatusBackend,
};

const EXTERNAL_DOCK_CSS: &str = include_str!("../assets/web/external-dock.css");
const HTMX: &str = include_str!("../assets/web/htmx.min.js");
const HTMX_WS: &str = include_str!("../assets/web/htmx-ext-ws.js");
const ARCH_LOGO: &[u8] = include_bytes!("../assets/web/arch-logo.png");
const PI_LOGO: &str = include_str!("../assets/web/pi-logo.svg");
const TODO_ICON: &str = include_str!("../assets/web/todo.svg");
static WEATHER_COORDS: OnceLock<(String, String)> = OnceLock::new();
static EVENTS: OnceLock<broadcast::Sender<String>> = OnceLock::new();
static EVENT_INPUT: OnceLock<mpsc::Sender<DockEvent>> = OnceLock::new();
static DOCK_DISPLAY: OnceLock<u32> = OnceLock::new();
static AGENT_STATE: OnceLock<Mutex<AgentWidgetState>> = OnceLock::new();
static CALENDAR_URLS: OnceLock<Vec<String>> = OnceLock::new();
static CALENDAR_EVENTS: OnceLock<Mutex<Vec<CalendarEvent>>> = OnceLock::new();
static SPECTRUM: OnceLock<Mutex<[u8; 16]>> = OnceLock::new();
static ARTWORK_CACHE: OnceLock<Mutex<HashMap<u64, Vec<u8>>>> = OnceLock::new();
static SPECTRUM_ACTIVE: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static AWAKE_GENERATION: AtomicU64 = AtomicU64::new(0);
static AWAKE_INHIBITOR: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
static DASHBOARD_SNAPSHOT: OnceLock<Mutex<String>> = OnceLock::new();
static BLUETOOTH_FRAGMENT: OnceLock<Mutex<FragmentCache>> = OnceLock::new();

const BLUETOOTH_DIALOG_ACTION: &str = "document.querySelector('#bluetooth-dialog').showModal()";
const BLUETOOTH_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><path d="M12 2v20M12 2l7 6-7 5M12 22l7-6-7-5M5 7l14 9M5 17 19 8"/></svg>"#;
const NETWORK_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><path d="M4 10a13 13 0 0 1 16 0M8 15a6.5 6.5 0 0 1 8 0"/><circle cx="12" cy="19" r="1.4"/></svg>"#;
const CONTROLLER_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><path d="M7 7h10a4 4 0 0 1 3.9 3.1l1.1 5.4a2.5 2.5 0 0 1-4.7 1.4l-1.2-2.4H7.9l-1.2 2.4A2.5 2.5 0 0 1 2 15.5l1.1-5.4A4 4 0 0 1 7 7Z"/><path d="M6 10v4M4 12h4M15.5 11.5h.01M18 13.5h.01"/></svg>"#;
const TRACKPAD_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 15h18"/></svg>"#;

#[derive(Default)]
struct AgentWidgetState {
    agents: Vec<AgentStatus>,
    completed: Vec<AgentStatus>,
    completed_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DockEvent {
    Volume,
    Media,
    InputMethod,
    Hyprland,
    DisplayPower,
    Agents,
    Calendar,
    Connectivity,
    Spectrum,
}

fn emit(event: DockEvent) {
    if let Some(events) = EVENT_INPUT.get() {
        let _ = events.send(event);
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
        let _ = CALENDAR_URLS.set(config.google_calendar_urls.clone());
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
        let (event_input, event_output) = mpsc::channel();
        let _ = EVENT_INPUT.set(event_input);
        thread::Builder::new()
            .name("raylander-dock-events".into())
            .spawn(move || {
                let mut previous = HashMap::<DockEvent, String>::new();
                while let Ok(event) = event_output.recv() {
                    let fragment = event_fragment(event);
                    if !fragment.is_empty() && previous.get(&event) != Some(&fragment) {
                        previous.insert(event, fragment.clone());
                        let _ = events.send(fragment);
                    }
                }
            })
            .expect("failed to start dock event renderer");
        start_event_sources();
        thread::Builder::new()
            .name("raylander-dock-warmup".into())
            .spawn(|| {
                let snapshot = dashboard_fragment();
                *DASHBOARD_SNAPSHOT
                    .get_or_init(|| Mutex::new(String::new()))
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = snapshot.clone();
                if let Some(events) = EVENTS.get() {
                    let _ = events.send(snapshot);
                }
            })
            .expect("failed to start dashboard warmup");
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
        .route("/media-visualizer/{active}", post(set_visualizer_active))
        .route("/volume/{percent}", post(set_volume))
        .route("/brightness/{percent}", post(set_brightness))
        .route("/awake/{active}", post(set_awake))
        .route("/input-method/toggle", post(toggle_input_method))
        .route("/bluetooth/power", post(bluetooth_power))
        .route("/bluetooth/scan", post(bluetooth_scan))
        .route(
            "/bluetooth/device/{address}/{action}",
            post(bluetooth_device),
        )
        .route("/agent/focus", post(focus_agent))
        .route("/agent/focus/{pane_id}", post(focus_agent_by_id))
        .route("/workspace/{id}", post(focus_workspace))
        .route("/weather-details", get(weather_details))
        .route("/location-name", get(location_name))
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
        .route("/assets/todo.svg", get(todo_icon))
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

async fn set_visualizer_active(
    axum::extract::Path(active): axum::extract::Path<bool>,
) -> StatusCode {
    SPECTRUM_ACTIVE
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .store(active, Ordering::Relaxed);
    if !active {
        *SPECTRUM
            .get_or_init(|| Mutex::new([0; 16]))
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = [0; 16];
        emit(DockEvent::Spectrum);
    }
    StatusCode::NO_CONTENT
}

async fn media_action(axum::extract::Path(action): axum::extract::Path<String>) -> StatusCode {
    let action = match action.as_str() {
        "previous" => MediaAction::Previous,
        "play" => MediaAction::Play,
        "pause" => MediaAction::Pause,
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

async fn bluetooth_power() -> StatusCode {
    let result = async {
        let state = BluetoothBackend.state().await?;
        let adapter = state.adapters.first().context("no Bluetooth adapter")?;
        BluetoothBackend
            .set_adapter_powered(&adapter.path, !adapter.powered)
            .await
    }
    .await;
    if result.is_ok() {
        emit(DockEvent::Connectivity);
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn bluetooth_scan() -> StatusCode {
    let result = async {
        let state = BluetoothBackend.state().await?;
        let adapter = state.adapters.first().context("no Bluetooth adapter")?;
        BluetoothBackend
            .set_discovering(&adapter.path, !adapter.discovering)
            .await
    }
    .await;
    if result.is_ok() {
        emit(DockEvent::Connectivity);
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn bluetooth_device(
    axum::extract::Path((address, action)): axum::extract::Path<(String, String)>,
) -> StatusCode {
    let result = match action.as_str() {
        "pair" => BluetoothBackend.pair_device(&address).await,
        "connect" => {
            BluetoothBackend
                .set_device_connected_by_address(&address, true)
                .await
        }
        "disconnect" => {
            BluetoothBackend
                .set_device_connected_by_address(&address, false)
                .await
        }
        _ => return StatusCode::NOT_FOUND,
    };
    if result.is_ok() {
        emit(DockEvent::Connectivity);
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn toggle_input_method() -> StatusCode {
    match tokio::task::spawn_blocking(|| Fcitx5Backend.apply(&InputMethodAction::Toggle)).await {
        Ok(Ok(())) => {
            emit(DockEvent::InputMethod);
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn set_awake(axum::extract::Path(active): axum::extract::Path<bool>) -> StatusCode {
    let generation = AWAKE_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let started = tokio::task::spawn_blocking(move || {
        let inhibitor = AWAKE_INHIBITOR.get_or_init(|| Mutex::new(None));
        let mut inhibitor = inhibitor.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(mut child) = inhibitor.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if !active {
            return Ok(());
        }
        *inhibitor = Some(
            Command::new("systemd-inhibit")
                .args([
                    "--what=idle",
                    "--mode=block",
                    "--who=Raylander",
                    "--why=30-minute awake timer",
                    "sleep",
                    "1800",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        Ok::<_, std::io::Error>(())
    })
    .await;
    if !matches!(started, Ok(Ok(()))) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        if AWAKE_GENERATION.load(Ordering::Relaxed) == generation
            && let Some(mut child) = AWAKE_INHIBITOR
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    });
    StatusCode::NO_CONTENT
}

async fn focus_agent_by_id(
    axum::extract::Path(pane_id): axum::extract::Path<String>,
) -> StatusCode {
    let focus_id = pane_id.clone();
    match tokio::task::spawn_blocking(move || HerdrBackend.focus(&focus_id)).await {
        Ok(Ok(())) => {
            if let Some(state) = AGENT_STATE.get() {
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.completed.retain(|agent| agent.pane_id != pane_id);
                if let Some(agent) = state
                    .agents
                    .iter_mut()
                    .find(|agent| agent.pane_id == pane_id && agent.status == "done")
                {
                    agent.status = "idle".to_owned();
                }
                if state.completed.is_empty()
                    && !state.agents.iter().any(|agent| agent.status == "done")
                {
                    state.completed_at = None;
                }
            }
            emit(DockEvent::Agents);
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn focus_agent() -> StatusCode {
    let agent = AGENT_STATE.get().and_then(|state| {
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        let pane_id = state
            .agents
            .iter()
            .find(|agent| agent.status == "blocked")
            .or_else(|| state.agents.iter().find(|agent| agent.status == "done"))
            .or_else(|| state.completed.first())
            .map(|agent| agent.pane_id.clone())?;
        state.completed.retain(|agent| agent.pane_id != pane_id);
        if let Some(agent) = state
            .agents
            .iter_mut()
            .find(|agent| agent.pane_id == pane_id)
        {
            agent.status = "idle".to_owned();
        }
        Some(pane_id)
    });
    let Some(pane_id) = agent else {
        return StatusCode::NO_CONTENT;
    };
    match tokio::task::spawn_blocking(move || HerdrBackend.focus(&pane_id)).await {
        Ok(Ok(())) => {
            emit(DockEvent::Agents);
            StatusCode::NO_CONTENT
        }
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
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Deserialize)]
struct LocationQuery {
    latitude: f64,
    longitude: f64,
}

async fn location_name(
    axum::extract::Query(query): axum::extract::Query<LocationQuery>,
) -> Response {
    if !(-90.0..=90.0).contains(&query.latitude) || !(-180.0..=180.0).contains(&query.longitude) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let url = format!(
        "https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat={}&lon={}",
        query.latitude, query.longitude
    );
    match tokio::task::spawn_blocking(move || {
        Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--max-time",
                "8",
                "--user-agent",
                "Raylander/0.1",
                &url,
            ])
            .output()
    })
    .await
    {
        Ok(Ok(output)) if output.status.success() => {
            let value: serde_json::Value = match serde_json::from_slice(&output.stdout) {
                Ok(value) => value,
                Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
            };
            let address = &value["address"];
            let name = ["city", "town", "village", "municipality", "county"]
                .iter()
                .find_map(|key| address[*key].as_str());
            match name {
                Some(name) => name
                    .strip_suffix(" County")
                    .unwrap_or(name)
                    .to_owned()
                    .into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn weather_details(
    axum::extract::Query(query): axum::extract::Query<WeatherDetailsQuery>,
) -> Response {
    let configured = WEATHER_COORDS.get();
    let phone_location = query
        .latitude
        .zip(query.longitude)
        .filter(|(latitude, longitude)| {
            (-90.0..=90.0).contains(latitude) && (-180.0..=180.0).contains(longitude)
        });
    let phone_coordinates =
        phone_location.map(|(latitude, longitude)| (latitude.to_string(), longitude.to_string()));
    let (latitude, longitude) = match query.city.as_deref().unwrap_or("minneapolis") {
        "minneapolis" => phone_coordinates
            .as_ref()
            .map(|(latitude, longitude)| (latitude.as_str(), longitude.as_str()))
            .or_else(|| {
                configured.map(|(latitude, longitude)| (latitude.as_str(), longitude.as_str()))
            })
            .unwrap_or(("44.9778", "-93.2650")),
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

#[derive(Deserialize)]
struct ArtworkQuery {
    t: Option<u64>,
}

async fn artwork(axum::extract::Query(query): axum::extract::Query<ArtworkQuery>) -> Response {
    let result = query.t.and_then(|key| {
        ARTWORK_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .cloned()
    });
    match result {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/*"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
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
    let snapshot = DASHBOARD_SNAPSHOT
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    if !snapshot.is_empty() && socket.send(Message::Text(snapshot.into())).await.is_err() {
        return;
    }
    // Send inexpensive live fragments without blocking on cold package, DDC, or weather queries.
    for event in [
        DockEvent::Volume,
        DockEvent::Media,
        DockEvent::InputMethod,
        DockEvent::Hyprland,
        DockEvent::DisplayPower,
        DockEvent::Agents,
        DockEvent::Calendar,
    ] {
        let fragment = tokio::task::spawn_blocking(move || event_fragment(event))
            .await
            .unwrap_or_default();
        if socket.send(Message::Text(fragment.into())).await.is_err() {
            return;
        }
    }
    loop {
        let Ok(fragment) = events.recv().await else {
            break;
        };
        if socket.send(Message::Text(fragment.into())).await.is_err() {
            break;
        }
    }
}

fn start_event_sources() {
    let volume_events = crate::native_observers::observe_volume();
    thread::spawn(move || {
        while volume_events.recv().is_ok() {
            emit(DockEvent::Volume);
        }
    });
    let media_events = crate::native_observers::observe_media();
    thread::spawn(move || {
        while media_events.recv().is_ok() {
            emit(DockEvent::Media);
        }
    });
    observe_connectivity();
    observe_input_method();
    observe_hyprland();
    observe_agents();
    observe_calendar();
    let spectrum_active = SPECTRUM_ACTIVE
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone();
    let spectrum = audio_visualizer::observe(spectrum_active);
    thread::spawn(move || {
        while let Ok(levels) = spectrum.recv() {
            *SPECTRUM
                .get_or_init(|| Mutex::new([0; 16]))
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = levels;
            emit(DockEvent::Spectrum);
        }
    });
}

fn observe_calendar() {
    thread::spawn(|| {
        loop {
            let urls = CALENDAR_URLS.get().cloned().unwrap_or_default();
            if let Ok(events) = GoogleCalendarBackend.events(&urls) {
                let state = CALENDAR_EVENTS.get_or_init(|| Mutex::new(Vec::new()));
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                if *state != events {
                    *state = events;
                    drop(state);
                    emit(DockEvent::Calendar);
                }
            }
            thread::sleep(Duration::from_secs(5 * 60));
        }
    });
}

fn observe_connectivity() {
    thread::spawn(|| {
        let mut previous: Option<BluetoothState> = None;
        loop {
            let current = zbus::block_on(BluetoothBackend.state()).ok();
            if current != previous {
                previous = current;
                emit(DockEvent::Connectivity);
            }
            thread::sleep(Duration::from_secs(2));
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
                emit(DockEvent::InputMethod);
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
                    .cloned()
                    .collect::<Vec<_>>();
                let state = AGENT_STATE.get_or_init(|| Mutex::new(AgentWidgetState::default()));
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.agents.clone_from(&current);
                if !completed.is_empty() {
                    for agent in completed {
                        if !state
                            .completed
                            .iter()
                            .any(|completed| completed.pane_id == agent.pane_id)
                        {
                            state.completed.push(agent);
                        }
                    }
                    state.completed_at = Some(Instant::now());
                }
                previous = current;
                drop(state);
                emit(DockEvent::Agents);
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
                    emit(DockEvent::Agents);
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
                if line.starts_with("workspace") || line.starts_with("focusedmon") {
                    emit(DockEvent::Hyprland);
                } else if line.starts_with("monitoradded")
                    || line.starts_with("monitorremoved")
                    || line.starts_with("configreloaded")
                {
                    emit(DockEvent::DisplayPower);
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

fn invalidate_fragment(cache: &'static OnceLock<Mutex<FragmentCache>>) {
    if let Some(cache) = cache.get() {
        cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .updated_at = None;
    }
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

fn connected_bluetooth_devices(state: &BluetoothState) -> Vec<&BluetoothDevice> {
    let mut devices: Vec<_> = state
        .devices
        .iter()
        .filter(|device| device.connected)
        .collect();
    devices.sort_by(|left, right| {
        bluetooth_device_priority(left)
            .cmp(&bluetooth_device_priority(right))
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.path.cmp(&right.path))
    });
    devices
}

fn bluetooth_device_priority(device: &BluetoothDevice) -> u8 {
    let name = device.name.to_ascii_lowercase();
    if name.contains("dualsense")
        || name.contains("playstation")
        || name.contains("controller")
        || name.contains("gamepad")
    {
        0
    } else if name.contains("trackpad") || name.contains("touchpad") {
        1
    } else {
        2
    }
}

fn bluetooth_device_glyph(device: &BluetoothDevice) -> &'static str {
    match bluetooth_device_priority(device) {
        0 => CONTROLLER_GLYPH,
        1 => TRACKPAD_GLYPH,
        _ => BLUETOOTH_GLYPH,
    }
}

fn bluetooth_device_control(device: &BluetoothDevice) -> String {
    let glyph = bluetooth_device_glyph(device);
    let battery = device
        .battery
        .map(|battery| {
            let class = if battery.percentage < 20 {
                "battery-fill low"
            } else {
                "battery-fill"
            };
            format!(
                r#"<span class="{class}" style="height:{}%"></span>"#,
                battery.percentage
            )
        })
        .unwrap_or_default();
    format!("{battery}{glyph}")
}

fn bluetooth_device_title(device: &BluetoothDevice) -> String {
    device.name.clone()
}

fn event_fragment(event: DockEvent) -> String {
    match event {
        DockEvent::Volume => volume_fragment(),
        DockEvent::Media => media_fragment(),
        DockEvent::InputMethod => input_method_fragment(),
        DockEvent::Hyprland => hyprland_oob_fragment(),
        DockEvent::DisplayPower => display_power_fragment(),
        DockEvent::Agents => agents_fragment(),
        DockEvent::Calendar => calendar_events_fragment(),
        DockEvent::Connectivity => {
            invalidate_fragment(&BLUETOOTH_FRAGMENT);
            dashboard_fragment()
        }
        DockEvent::Spectrum => spectrum_fragment(),
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
        Some(InputMethodStatus::Active) => "control-icon input-icon active",
        Some(InputMethodStatus::Inactive) => "control-icon input-icon",
        _ => "control-icon input-icon warning",
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

fn spectrum_fragment() -> String {
    let levels = *SPECTRUM
        .get_or_init(|| Mutex::new([0; 16]))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let levels = levels
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"<div id="spectrum-data" data-levels="{levels}" hidden hx-swap-oob="outerHTML"></div>"#
    )
}

fn media_primary_control(status: PlaybackStatus) -> &'static str {
    if status == PlaybackStatus::Playing {
        r#"<button class="media-primary playing" onclick="fetch('/media/pause',{method:'POST'})" aria-label="Pause"><svg class="play-icon" viewBox="0 0 24 24"><path d="m8 5 11 7-11 7z"/></svg><svg class="pause-icon" viewBox="0 0 24 24"><path d="M7 5h4v14H7zM13 5h4v14h-4z"/></svg></button>"#
    } else {
        r#"<button class="media-primary" onclick="fetch('/media/play',{method:'POST'})" aria-label="Play"><svg class="play-icon" viewBox="0 0 24 24"><path d="m8 5 11 7-11 7z"/></svg><svg class="pause-icon" viewBox="0 0 24 24"><path d="M7 5h4v14H7zM13 5h4v14h-4z"/></svg></button>"#
    }
}

fn media_art_key(media: &crate::media::MediaState) -> u64 {
    let mut hasher = DefaultHasher::new();
    media.player.hash(&mut hasher);
    media.title.hash(&mut hasher);
    media.artwork_url.hash(&mut hasher);
    let key = hasher.finish();
    if let Some(url) = media.artwork_url.as_deref() {
        let cache = ARTWORK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let needs_load = !cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&key);
        if needs_load && let Some(bytes) = load_artwork(url) {
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            if cache.len() >= 12 {
                cache.clear();
            }
            cache.insert(key, bytes);
        }
    }
    key
}

fn load_artwork(url: &str) -> Option<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).ok();
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        let output = Command::new("curl")
            .args(["--fail", "--silent", "--max-time", "5", url])
            .output()
            .ok()?;
        return output.status.success().then_some(output.stdout);
    }
    None
}

fn media_fullscreen(media: &crate::media::MediaState) -> String {
    let art_key = media_art_key(media);
    let primary = media_primary_control(media.status);
    let progress = match (media.position_micros, media.duration_micros) {
        (Some(position), Some(duration)) if duration > 0 => {
            (position.saturating_mul(100) / duration).min(100)
        }
        _ => 0,
    };
    let playing = if media.status == PlaybackStatus::Playing {
        " playing"
    } else {
        ""
    };
    format!(
        r#"<div id="media-full-panel" class="media-full-panel no-lyrics{playing}" hx-swap-oob="outerHTML"><div class="media-full-ambient"><i></i><i></i><i></i></div><div id="media-visualizer" class="media-visualizer"><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i></div><section class="media-full-left"><img src="/artwork?t={art_key}" alt="Album artwork"><h2>{}</h2><p>{}</p><div class="media-progress-wrap"><div class="media-progress"><i style="width:{progress}%"></i></div></div><div class="media-full-controls"><button onclick="fetch('/media/previous',{{method:'POST'}})"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button>{primary}<button onclick="fetch('/media/next',{{method:'POST'}})"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></section></div>"#,
        escape(&media.title),
        escape(&media.artists.join(", "))
    )
}

fn media_fragment() -> String {
    let mut html = String::new();
    match PlayerctlBackend.current() {
        Ok(Some(media)) => {
            let playing = if media.status == PlaybackStatus::Playing { " playing" } else { "" };
            let primary_action = media_primary_control(media.status);
            html.push_str(&format!(r#"<div id="media-ambient" class="media-ambient{playing}" hx-swap-oob="outerHTML"><i></i><i></i><i></i></div>"#));
            html.push_str(&format!(r#"<div id="media-empty" class="media-empty hidden" hx-swap-oob="outerHTML"></div><div id="media-copy" class="media-copy" hx-swap-oob="outerHTML"><p id="media-title" class="value truncate">{}</p><p id="media-artist" class="truncate text-[9px] text-[#a6acb9]">{}</p><div class="media-controls" onclick="event.stopPropagation()"><button onclick="fetch('/media/previous',{{method:'POST'}})" aria-label="Previous"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button>{primary_action}<button onclick="fetch('/media/next',{{method:'POST'}})" aria-label="Next"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></div>"#, escape(&media.title), escape(&media.artists.join(", "))));
            if media.artwork_url.is_some() { html.push_str(&format!(r#"<img id="media-art" src="/artwork?t={}" alt="" hx-swap-oob="outerHTML" onerror="this.hidden=true" onload="this.hidden=false">"#, media_art_key(&media))); }
            html.push_str(&media_fullscreen(&media));
        }
        _ => html.push_str(r#"<div id="media-ambient" class="media-ambient" hx-swap-oob="outerHTML"></div><img id="media-art" hidden hx-swap-oob="outerHTML" alt=""><div id="media-empty" class="media-empty" hx-swap-oob="outerHTML" aria-hidden="true"><img src="/assets/arch-logo.png" alt=""></div><div id="media-copy" class="media-copy hidden" hx-swap-oob="outerHTML"></div>"#),
    }
    html
}

fn calendar_events_fragment() -> String {
    let state = CALENDAR_EVENTS.get_or_init(|| Mutex::new(Vec::new()));
    let events = state.lock().unwrap_or_else(|error| error.into_inner());
    let today = Local::now().date_naive();
    let upcoming = events
        .iter()
        .filter(|event| event.start.date_naive() >= today)
        .collect::<Vec<_>>();
    let compact = upcoming
        .iter()
        .filter(|event| event.start.date_naive() <= today.succ_opt().unwrap_or(today))
        .map(|event| format!(r#"<div class="calendar-event-mini"><i></i><span><b>{}</b><small>{}</small></span></div>"#, escape(&event.title), event_time(event)))
        .collect::<String>();
    let details = upcoming
        .iter()
        .map(|event| {
            let action = if event.url.is_empty() {
                String::new()
            } else {
                format!(
                    r#" onclick="window.open('{}','_blank')" role="link""#,
                    escape(&event.url)
                )
            };
            format!(r#"<article class="calendar-event-detail"{action}><time><b>{}</b><small>{}</small></time><div><strong>{}</strong><span>{}</span></div></article>"#, event.start.format("%a"), event_time(event), escape(&event.title), escape(&event.location))
        })
        .collect::<String>();
    let empty = if compact.is_empty() {
        r#"<div class="calendar-empty"><img src="/assets/todo.svg" alt="No upcoming events"><span>No upcoming events</span></div>"#
    } else {
        ""
    };
    let empty_class = if compact.is_empty() { " empty" } else { "" };
    format!(
        r#"<section id="calendar-events-card" class="card calendar-events-card{empty_class}" onclick="if(this.dataset.scrolled){{delete this.dataset.scrolled}}else{{document.querySelector('#calendar-events-dialog').showModal()}}" role="button" hx-swap-oob="outerHTML"><div onscroll="this.parentElement.dataset.scrolled='1'">{compact}{empty}</div></section><div id="calendar-events-panel" class="calendar-events-panel" hx-swap-oob="outerHTML"><header><span><b>{}</b><small>{}</small></span><h2>Upcoming</h2></header><main class="{empty_class}">{details}{empty}</main></div>"#,
        Local::now().day(),
        Local::now().format("%b")
    )
}

fn event_time(event: &CalendarEvent) -> String {
    if event.all_day {
        "All day".into()
    } else {
        let hour = event.start.hour();
        let suffix = if hour < 12 { "AM" } else { "PM" };
        format!(
            "{}:{:02} {suffix}",
            if hour.is_multiple_of(12) {
                12
            } else {
                hour % 12
            },
            event.start.minute()
        )
    }
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
        if !completed_agents
            .iter()
            .any(|item| item.pane_id == agent.pane_id)
        {
            completed_agents.push(agent.clone());
        }
    }
    let completed = !completed_agents.is_empty()
        && (state.agents.iter().any(|agent| agent.status == "done")
            || state
                .completed_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(10)));
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
                .map(|agent| (agent.workspace.as_str(), agent.pane.as_str(), "done"))
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
    let action = if !blocked.is_empty() || completed {
        r#" onclick="fetch('/agent/focus',{method:'POST'})" role="button""#
    } else {
        r#" onclick="openAgents()" role="button""#
    };
    let all_agents = state
        .agents
        .iter()
        .map(|agent| {
            let status = escape(&agent.status);
            format!(
                r#"<button class="agent-detail {status}" onclick="focusAgent('{}')"><span><strong>{}</strong><small>· {}</small></span><b>{status}</b></button>"#,
                escape(&agent.pane_id),
                escape(&agent.workspace),
                escape(&agent.pane)
            )
        })
        .collect::<String>();
    format!(
        r#"<section id="agent-card" class="{class}"{action} hx-swap-oob="outerHTML"><div class="agent-list">{rows}{idle_logo}</div></section><div id="agent-dialog-content" class="agent-dialog-content" hx-swap-oob="outerHTML"><aside><img src="/assets/pi-logo.svg" alt="Pi"></aside><section>{all_agents}</section></div>"#
    )
}

fn dashboard_fragment() -> String {
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
        &BLUETOOTH_FRAGMENT,
        Duration::from_millis(400),
        || {
            let state = zbus::block_on(BluetoothBackend.state()).ok()?;
            let connected_devices = connected_bluetooth_devices(&state);
            let bluetooth_class = if connected_devices.is_empty() {
                match &state.availability {
                    BluetoothAvailability::Unavailable { .. } => "control-icon",
                    BluetoothAvailability::Available
                        if state.adapters.iter().any(|adapter| adapter.powered) =>
                    {
                        "control-icon active"
                    }
                    BluetoothAvailability::Available => "control-icon",
                }
            } else {
                "control-icon device-control"
            };
            let bluetooth_value = connected_devices
                .first()
                .map(|device| bluetooth_device_control(device))
                .unwrap_or_else(|| BLUETOOTH_GLYPH.to_owned());
            let bluetooth_title = connected_devices
                .first()
                .map(|device| bluetooth_device_title(device))
                .unwrap_or_else(|| "Bluetooth".to_owned());
            let network = NmcliBackend.current().ok();
            let network_class = match network {
                Some(ref state) if !state.connections.is_empty() => "control-icon connected",
                Some(ref state) if state.wifi_enabled => "control-icon active",
                _ => "control-icon",
            };
            let input = Fcitx5Backend.current().ok();
            let input_class = match input.as_ref().map(|state| &state.status) {
                Some(InputMethodStatus::Active) => "control-icon input-icon active",
                Some(InputMethodStatus::Inactive) => "control-icon input-icon",
                _ => "control-icon input-icon warning",
            };
            let input_label = input
                .and_then(|state| state.name)
                .map(|name| if name.contains("pinyin") { "中" } else { "A" })
                .unwrap_or("?");
            let mut fragment = String::new();
            class_replace_with_action(
                &mut fragment,
                "bluetooth-control",
                bluetooth_class,
                &bluetooth_value,
                BLUETOOTH_DIALOG_ACTION,
                &bluetooth_title,
            );
            if let Some(device) = connected_devices.get(1) {
                let value = bluetooth_device_control(device);
                let title = bluetooth_device_title(device);
                class_replace_with_action(
                    &mut fragment,
                    "network-control",
                    "control-icon device-control",
                    &value,
                    BLUETOOTH_DIALOG_ACTION,
                    &title,
                );
            } else {
                class_replace(&mut fragment, "network-control", network_class, NETWORK_GLYPH);
            }
            class_replace(&mut fragment, "input-control", input_class, input_label);
            let powered = state.adapters.first().is_some_and(|adapter| adapter.powered);
            let discovering = state.adapters.first().is_some_and(|adapter| adapter.discovering);
            let devices = state.devices.iter().map(|device| {
                let action = if device.connected { "disconnect" } else if device.paired { "connect" } else { "pair" };
                let label = if device.connected { "Disconnect" } else if device.paired { "Connect" } else { "Pair" };
                format!(r#"<article class="connectivity-item bluetooth-device"><span><b>{}</b><small>{}</small></span><button onclick="event.stopPropagation();bluetoothAction('{}','{}',this)">{}</button></article>"#, escape(&device.name), escape(&device.address), escape(&device.address), action, label)
            }).collect::<String>();
            let devices = if devices.is_empty() { r#"<p class="connectivity-empty">No devices found</p>"#.to_owned() } else { devices };
            fragment.push_str(&format!(r#"<div id="bluetooth-panel" class="connectivity-panel" hx-swap-oob="outerHTML"><header><h2>Bluetooth</h2><div class="connectivity-actions"><button onclick="bluetoothCommand('power',this)">{}</button><button onclick="bluetoothCommand('scan',this)">{}</button></div></header><main>{devices}</main></div>"#, if powered { "Turn off" } else { "Turn on" }, if discovering { "Stop scan" } else { "Add device" }));
            let mut networks = network.as_ref().map(|state| state.connections.iter().filter(|connection| connection.device == "enp5s0" || connection.kind.contains("ethernet")).map(|connection| format!(r#"<article class="connectivity-item connected"><span><b>{}</b><small>{}</small></span><em>Wired · Connected</em></article>"#, escape(&connection.name), escape(&connection.device))).collect::<String>()).unwrap_or_default();
            if let Ok(output) = Command::new("tailscale").args(["status", "--json"]).output() && output.status.success() && let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                let online = status.pointer("/Self/Online").and_then(serde_json::Value::as_bool).unwrap_or(false);
                let name = status.pointer("/Self/DNSName").and_then(serde_json::Value::as_str).unwrap_or("Tailscale").trim_end_matches('.');
                networks.push_str(&format!(r#"<article class="connectivity-item tailscale"><span><b>Tailscale</b><small>{}</small></span><em>{}</em></article>"#, escape(name), if online { "Connected" } else { "Offline" }));
            }
            networks.push_str(&Command::new("nmcli").args(["--terse", "--fields", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "yes"]).output().ok().filter(|output| output.status.success()).map(|output| String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| { let mut fields = line.splitn(4, ':'); let active = fields.next()? == "*"; let name = fields.next()?; let signal = fields.next()?; let security = fields.next().unwrap_or_default(); (!name.is_empty()).then(|| format!(r#"<article class="connectivity-item"><span><b>{}</b><small>{}</small></span><em>{}%{}</em></article>"#, escape(name), escape(security), escape(signal), if active { " · Connected" } else { "" })) }).collect::<String>()).unwrap_or_default());
            let networks = if networks.is_empty() { r#"<p class="connectivity-empty">No networks</p>"#.to_owned() } else { networks };
            fragment.push_str(&format!(r#"<div id="network-panel" class="connectivity-panel" hx-swap-oob="outerHTML"><header><h2>Wi-Fi</h2><span>Networks</span></header><main>{networks}</main></div>"#));
            Some(fragment)
        },
    ));
    match PlayerctlBackend.current() {
        Ok(Some(media)) => {
            let playing = if media.status == PlaybackStatus::Playing {
                " playing"
            } else {
                ""
            };
            let primary_action = media_primary_control(media.status);
            html.push_str(&format!(r#"<div id="media-ambient" class="media-ambient{playing}" hx-swap-oob="outerHTML"><i></i><i></i><i></i></div>"#));
            replace(&mut html, "media-title", &media.title);
            replace(&mut html, "media-artist", &media.artists.join(", "));
            html.push_str(&format!(r#"<div id="media-empty" class="media-empty hidden" hx-swap-oob="outerHTML"></div><div id="media-copy" class="media-copy" hx-swap-oob="outerHTML"><p id="media-title" class="value truncate"></p><p id="media-artist" class="truncate text-[9px] text-[#a6acb9]"></p><div class="media-controls" onclick="event.stopPropagation()"><button onclick="fetch('/media/previous',{{method:'POST'}})" aria-label="Previous"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button>{primary_action}<button onclick="fetch('/media/next',{{method:'POST'}})" aria-label="Next"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></div>"#));
            if media.artwork_url.is_some() {
                html.push_str(&format!(r#"<img id="media-art" src="/artwork?t={}" alt="" hx-swap-oob="outerHTML" onerror="this.hidden=true" onload="this.hidden=false">"#, media_art_key(&media)));
            }
            html.push_str(&media_fullscreen(&media));
        }
        _ => {
            html.push_str(r#"<div id="media-ambient" class="media-ambient" hx-swap-oob="outerHTML"></div><img id="media-art" hidden hx-swap-oob="outerHTML" alt=""><div id="media-empty" class="media-empty" hx-swap-oob="outerHTML" aria-hidden="true"><img src="/assets/arch-logo.png" alt=""></div><div id="media-copy" class="media-copy hidden" hx-swap-oob="outerHTML"></div>"#);
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
    html.push_str(&calendar_events_fragment());
    html.push_str(&spectrum_fragment());
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
    let (action, title) = match id {
        "bluetooth-control" => (BLUETOOTH_DIALOG_ACTION, "Bluetooth"),
        "network-control" => (
            "document.querySelector('#network-dialog').showModal()",
            "Wi-Fi",
        ),
        "input-control" => (
            "fetch('/input-method/toggle',{method:'POST'})",
            "Input method",
        ),
        _ => ("", ""),
    };
    class_replace_with_action(html, id, class, value, action, title);
}

fn class_replace_with_action(
    html: &mut String,
    id: &str,
    class: &str,
    value: &str,
    action: &str,
    title: &str,
) {
    let action = if action.is_empty() {
        String::new()
    } else {
        format!(r#" onclick="{action}""#)
    };
    html.push_str(&format!(
        r#"<span id="{id}" class="{class}" title="{}"{action} hx-swap-oob="outerHTML">{value}</span>"#,
        escape(title),
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

async fn todo_icon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], TODO_ICON)
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
        assert!(page.contains("HTMX will reconnect"));
        assert!(page.contains("toLocaleTimeString"));
    }

    fn bluetooth_test_device(
        path: &str,
        name: &str,
        connected: bool,
        battery: Option<u8>,
    ) -> BluetoothDevice {
        BluetoothDevice {
            path: path.into(),
            adapter_path: None,
            address: path.into(),
            name: name.into(),
            paired: true,
            connected,
            trusted: true,
            battery: battery.map(|percentage| crate::bluetooth::BluetoothBattery { percentage }),
        }
    }

    #[test]
    fn connected_bluetooth_devices_put_controller_before_trackpad() {
        let state = BluetoothState {
            availability: BluetoothAvailability::Available,
            adapters: Vec::new(),
            devices: vec![
                bluetooth_test_device("trackpad", "Magic Trackpad", true, Some(42)),
                bluetooth_test_device(
                    "controller",
                    "DualSense Wireless Controller",
                    true,
                    Some(95),
                ),
            ],
        };
        let devices = connected_bluetooth_devices(&state);
        assert_eq!(devices[0].name, "DualSense Wireless Controller");
        assert_eq!(devices[1].name, "Magic Trackpad");
        let controller = bluetooth_device_control(devices[0]);
        assert!(controller.contains("class=\"battery-fill\" style=\"height:95%\""));
        assert!(!controller.contains("<small>"));
        assert!(controller.contains(CONTROLLER_GLYPH));
        assert!(bluetooth_device_control(devices[1]).contains(TRACKPAD_GLYPH));
        assert!(
            bluetooth_device_control(&bluetooth_test_device(
                "low",
                "DualSense Wireless Controller",
                true,
                Some(19)
            ))
            .contains("class=\"battery-fill low\" style=\"height:19%\"")
        );
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
