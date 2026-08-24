use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
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
use tokio::sync::{broadcast, oneshot};

use crate::{
    components::{
        agents, calendar_events, connectivity, controls, dock, focus, media, packages,
        system_status, weather,
    },
    config::ComponentsConfig,
};

const EXTERNAL_DOCK_CSS: &str = include_str!("../../../assets/web/external-dock.css");
const HTMX: &str = include_str!("../../../assets/web/htmx.min.js");
const HTMX_WS: &str = include_str!("../../../assets/web/htmx-ext-ws.js");
const DOCK_JS: &str = include_str!("dock.js");
static TRANSPORT: OnceLock<FragmentTransport> = OnceLock::new();
static DASHBOARD_SNAPSHOT: OnceLock<Mutex<String>> = OnceLock::new();

#[derive(Clone, Copy)]
struct Invalidation {
    key: &'static str,
    render: fn() -> String,
}

#[derive(Clone)]
struct ComponentContext {
    invalidations: mpsc::Sender<Invalidation>,
    shutdown: Arc<AtomicBool>,
}

impl ComponentContext {
    fn invalidate(&self, key: &'static str, render: fn() -> String) {
        if !self.shutdown.load(Ordering::Relaxed) {
            let _ = self.invalidations.send(Invalidation { key, render });
        }
    }
}

#[derive(Clone)]
struct FragmentTransport(broadcast::Sender<String>);

impl FragmentTransport {
    fn broadcast(&self, fragment: String) {
        let _ = self.0.send(fragment);
    }

    fn subscribe(&self) -> broadcast::Receiver<String> {
        self.0.subscribe()
    }
}

#[derive(Template)]
#[template(path = "external-dock.html")]
struct DockTemplate {
    dashboard: String,
    overlays: String,
}

pub struct WebServer {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl WebServer {
    pub fn start(config: &ComponentsConfig) -> Result<Self> {
        let address = SocketAddr::new(config.external_dock.bind_address, config.external_dock.port);
        dock::configure(config)?;
        weather::configure(weather::WeatherConfig {
            latitude: config.weather.latitude.clone(),
            longitude: config.weather.longitude.clone(),
        });
        calendar_events::configure(calendar_events::CalendarEventsConfig {
            urls: config.calendar_events.urls.clone(),
        });
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
        let transport = FragmentTransport(events);
        let _ = TRANSPORT.set(transport.clone());
        let (event_input, event_output) = mpsc::channel();
        let shutdown_token = Arc::new(AtomicBool::new(false));
        let component_context = ComponentContext {
            invalidations: event_input,
            shutdown: Arc::clone(&shutdown_token),
        };
        let renderer_shutdown = Arc::clone(&shutdown_token);
        let renderer = thread::Builder::new()
            .name("raylander-dock-events".into())
            .spawn(move || {
                let mut previous = HashMap::<&'static str, String>::new();
                while !renderer_shutdown.load(Ordering::Relaxed) {
                    let Ok(event) =
                        event_output.recv_timeout(std::time::Duration::from_millis(250))
                    else {
                        continue;
                    };
                    let fragment = (event.render)();
                    if !fragment.is_empty() && previous.get(event.key) != Some(&fragment) {
                        previous.insert(event.key, fragment.clone());
                        transport.broadcast(fragment);
                    }
                }
            })
            .expect("failed to start dock event renderer");
        let component_tasks = start_component_tasks(component_context);
        thread::Builder::new()
            .name("raylander-dock-warmup".into())
            .spawn(|| {
                let snapshot = dock::update_snapshot();
                *DASHBOARD_SNAPSHOT
                    .get_or_init(|| Mutex::new(String::new()))
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = snapshot.clone();
                if let Some(transport) = TRANSPORT.get() {
                    transport.broadcast(snapshot);
                }
            })
            .expect("failed to start dashboard warmup");
        let _ = ready.send(Ok(()));
        let graceful_shutdown_token = Arc::clone(&shutdown_token);
        if let Err(error) = axum::serve(listener, router())
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
                graceful_shutdown_token.store(true, Ordering::Relaxed);
            })
            .await
        {
            tracing::warn!(%error, "external dock server stopped unexpectedly");
        }
        shutdown_token.store(true, Ordering::Relaxed);
        for task in component_tasks {
            let _ = task.join();
        }
        let _ = renderer.join();
    });
}

fn router() -> Router {
    let component_routes = dock::registry()
        .expect("dock component registry must be valid")
        .router();
    Router::new()
        .route("/", get(index))
        .route("/ws", get(websocket))
        .merge(component_routes)
        .route(
            "/assets/external-dock.css",
            get(|| async { stylesheet(EXTERNAL_DOCK_CSS) }),
        )
        .route("/assets/htmx.min.js", get(|| async { javascript(HTMX) }))
        .route(
            "/assets/htmx-ext-ws.js",
            get(|| async { javascript(HTMX_WS) }),
        )
        .route("/assets/dock.js", get(|| async { javascript(DOCK_JS) }))
}

async fn index() -> Response {
    let composition = match dock::compose() {
        Ok(composition) => composition,
        Err(error) => {
            tracing::error!(%error, "failed to compose external dock components");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    match (DockTemplate {
        dashboard: composition.dashboard,
        overlays: composition.overlays,
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
    let Some(transport) = TRANSPORT.get() else {
        return;
    };
    let mut events = transport.subscribe();
    let snapshot = DASHBOARD_SNAPSHOT
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    if !snapshot.is_empty() && socket.send(Message::Text(snapshot.into())).await.is_err() {
        return;
    }
    // Send inexpensive live fragments without blocking on cold package, DDC, or weather queries.
    for invalidation in initial_invalidations() {
        let fragment = tokio::task::spawn_blocking(move || (invalidation.render)())
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

fn start_component_tasks(context: ComponentContext) -> Vec<JoinHandle<()>> {
    let notify = |key, render, context: &ComponentContext| {
        let context = context.clone();
        move || context.invalidate(key, render)
    };
    let shutdown = &context.shutdown;
    vec![
        controls::observe_volume(
            Arc::clone(shutdown),
            notify("controls/volume", controls::volume_fragment, &context),
        ),
        media::observe_media(
            Arc::clone(shutdown),
            notify("media/main", media::update_fragment, &context),
        ),
        connectivity::observe_bluetooth(Arc::clone(shutdown), {
            let context = context.clone();
            move || {
                connectivity::invalidate();
                context.invalidate("connectivity/main", connectivity::update_fragment);
            }
        }),
        connectivity::observe_input_method(
            Arc::clone(shutdown),
            notify(
                "connectivity/input-method",
                connectivity::input_method_fragment,
                &context,
            ),
        ),
        focus::observe(
            Arc::clone(shutdown),
            notify("focus/main", focus::oob_fragment, &context),
            notify(
                "controls/display-power",
                controls::update_fragment,
                &context,
            ),
        ),
        agents::observe(
            Arc::clone(shutdown),
            notify("agents/main", agents::update_fragment, &context),
        ),
        calendar_events::observe(
            Arc::clone(shutdown),
            notify(
                "calendar-events/main",
                calendar_events::update_fragment,
                &context,
            ),
        ),
        packages::observe(
            Arc::clone(shutdown),
            notify("packages/main", packages::update_fragment, &context),
        ),
        system_status::observe(
            Arc::clone(shutdown),
            notify(
                "system-status/main",
                system_status::update_fragment,
                &context,
            ),
        ),
        media::observe_spectrum(
            Arc::clone(shutdown),
            notify("media/spectrum", media::spectrum_fragment, &context),
        ),
    ]
}

fn initial_invalidations() -> [Invalidation; 7] {
    [
        Invalidation {
            key: "controls/volume",
            render: controls::volume_fragment,
        },
        Invalidation {
            key: "media/main",
            render: media::update_fragment,
        },
        Invalidation {
            key: "connectivity/input-method",
            render: connectivity::input_method_fragment,
        },
        Invalidation {
            key: "focus/main",
            render: focus::oob_fragment,
        },
        Invalidation {
            key: "controls/display-power",
            render: controls::update_fragment,
        },
        Invalidation {
            key: "agents/main",
            render: agents::update_fragment,
        },
        Invalidation {
            key: "calendar-events/main",
            render: calendar_events::update_fragment,
        },
    ]
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::components::connectivity::{
        CONTROLLER_GLYPH, TRACKPAD_GLYPH,
        bluetooth::{BluetoothAvailability, BluetoothBackend, BluetoothDevice, BluetoothState},
        bluetooth_device_control, connected_bluetooth_devices,
    };

    use super::*;

    #[test]
    fn demo_page_contains_htmx_websocket_and_greeting() {
        let composition = dock::compose().unwrap();
        let page = DockTemplate {
            dashboard: composition.dashboard,
            overlays: composition.overlays,
        }
        .render()
        .unwrap();
        assert!(page.contains("id=\"local-time\""));
        assert!(page.contains("id=\"system-card\""));
        assert!(page.contains("id=\"weather-card\""));
        assert!(page.contains("/assets/external-dock.css"));
        assert!(page.contains("class=\"min-h-screen"));
        assert!(!page.contains("<style>"));
        assert!(page.contains("hx-ext=\"ws\""));
        assert!(page.contains("ws-connect=\"/ws\""));
        assert!(page.contains("/assets/dock.js"));
        assert!(DOCK_JS.contains("htmx:wsClose"));
        assert!(DOCK_JS.contains("HTMX will reconnect"));
        assert!(include_str!("../clock/component.js").contains("toLocaleTimeString"));
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
            battery: battery.map(|percentage| {
                crate::components::connectivity::bluetooth::BluetoothBattery { percentage }
            }),
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
        weather::configure(weather::WeatherConfig {
            latitude: "30.2672".into(),
            longitude: "-97.7431".into(),
        });
        type BenchmarkCase = (&'static str, Box<dyn Fn()>);
        let cases: [BenchmarkCase; 9] = [
            (
                "volume",
                Box::new(|| {
                    let _ = crate::components::controls::audio::WpctlBackend.current();
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
                    let _ = crate::components::media::backend::PlayerctlBackend.current();
                }),
            ),
            (
                "packages",
                Box::new(|| {
                    let _ = crate::components::packages::backend::PackageBackend.current();
                }),
            ),
            (
                "window",
                Box::new(|| {
                    let _ = focus::update_fragment();
                }),
            ),
            (
                "brightness",
                Box::new(|| {
                    let _ = crate::components::controls::display::DdcutilBackend.all_brightness();
                }),
            ),
            (
                "system",
                Box::new(|| {
                    let _ =
                        crate::components::system_status::backend::SystemStatusBackend.current();
                }),
            ),
            (
                "weather",
                Box::new(|| {
                    let _ = weather::update_fragment();
                }),
            ),
            (
                "full dashboard",
                Box::new(|| {
                    let _ = dock::update_snapshot();
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
