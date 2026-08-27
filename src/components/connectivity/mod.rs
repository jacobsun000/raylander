use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Router,
    extract::Path,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};

use crate::components::GridSize;

use self::{
    bluetooth::{BluetoothAvailability, BluetoothBackend, BluetoothDevice, BluetoothState},
    inhibitor::AwakeBackend,
    input_method::{Fcitx5Backend, InputMethodAction, InputMethodStatus},
    network::NmcliBackend,
};
use super::dock_component;

#[cfg_attr(target_os = "windows", path = "bluetooth_windows.rs")]
pub(crate) mod bluetooth;
#[cfg_attr(target_os = "windows", path = "inhibitor_windows.rs")]
mod inhibitor;
#[cfg_attr(target_os = "windows", path = "input_method_windows.rs")]
pub(crate) mod input_method;
#[cfg_attr(target_os = "windows", path = "network_windows.rs")]
pub(crate) mod network;

dock_component!(Connectivity, "connectivity", GridSize::ONE_BY_ONE, routes());

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/component.js", get(component_javascript))
        .route("/bluetooth/power", post(bluetooth_power))
        .route("/bluetooth/scan", post(bluetooth_scan))
        .route(
            "/bluetooth/device/{address}/{action}",
            post(bluetooth_device),
        )
        .route("/input-method/toggle", post(toggle_input_method))
        .route("/awake/{active}", post(set_awake))
}

async fn component_javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("component.js"),
    )
}

pub(crate) fn observe_bluetooth(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("raylander-bluetooth".into())
        .spawn(move || {
            let mut previous = None;
            while !shutdown.load(Ordering::Relaxed) {
                let current = bluetooth_state_blocking().ok();
                if current != previous {
                    previous = current;
                    on_change();
                }
                thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("failed to start Bluetooth observer")
}

fn bluetooth_state_blocking() -> anyhow::Result<BluetoothState> {
    #[cfg(target_os = "linux")]
    {
        zbus::block_on(BluetoothBackend.state())
    }
    #[cfg(target_os = "windows")]
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create Bluetooth query runtime")?
            .block_on(BluetoothBackend.state())
    }
}

pub(crate) fn observe_input_method(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("raylander-input-method".into())
        .spawn(move || {
            let mut previous = None;
            while !shutdown.load(Ordering::Relaxed) {
                let current = Fcitx5Backend.current().ok();
                if previous.as_ref() != current.as_ref() {
                    previous = current;
                    on_change();
                }
                thread::sleep(Duration::from_millis(500));
            }
        })
        .expect("failed to start input method observer")
}

pub(crate) fn input_method_fragment() -> String {
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
    format!(
        r#"<button id="input-control" class="{class}" title="Input method" onclick="fetch('/components/connectivity/input-method/toggle',{{method:'POST'}})" hx-swap-oob="outerHTML">{label}</button>"#
    )
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
    status(result)
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
    status(result)
}

async fn bluetooth_device(Path((address, action)): Path<(String, String)>) -> StatusCode {
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
    status(result)
}

async fn toggle_input_method() -> StatusCode {
    match tokio::task::spawn_blocking(|| Fcitx5Backend.apply(&InputMethodAction::Toggle)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn set_awake(Path(active): Path<bool>) -> StatusCode {
    let Ok(Ok(generation)) = tokio::task::spawn_blocking(move || AwakeBackend.set(active)).await
    else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        AwakeBackend.expire(generation);
    });
    StatusCode::NO_CONTENT
}

fn status(result: anyhow::Result<()>) -> StatusCode {
    if result.is_ok() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

const BLUETOOTH_DIALOG_ACTION: &str = "document.querySelector('#bluetooth-dialog').showModal()";
const BLUETOOTH_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><path d="M12 2v20M12 2l7 6-7 5M12 22l7-6-7-5M5 7l14 9M5 17 19 8"/></svg>"#;
const NETWORK_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><path d="M4 10a13 13 0 0 1 16 0M8 15a6.5 6.5 0 0 1 8 0"/><circle cx="12" cy="19" r="1.4"/></svg>"#;
pub(crate) const CONTROLLER_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><path d="M7 7h10a4 4 0 0 1 3.9 3.1l1.1 5.4a2.5 2.5 0 0 1-4.7 1.4l-1.2-2.4H7.9l-1.2 2.4A2.5 2.5 0 0 1 2 15.5l1.1-5.4A4 4 0 0 1 7 7Z"/><path d="M6 10v4M4 12h4M15.5 11.5h.01M18 13.5h.01"/></svg>"#;
pub(crate) const TRACKPAD_GLYPH: &str = r#"<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 15h18"/></svg>"#;
static FRAGMENT: OnceLock<Mutex<FragmentCache>> = OnceLock::new();

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

pub(crate) fn connected_bluetooth_devices(state: &BluetoothState) -> Vec<&BluetoothDevice> {
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

pub(crate) fn bluetooth_device_control(device: &BluetoothDevice) -> String {
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

pub(crate) fn invalidate() {
    invalidate_fragment(&FRAGMENT);
}

pub(crate) fn update_fragment() -> String {
    cached_fragment(&FRAGMENT, Duration::from_millis(400), || {
        let state = bluetooth_state_blocking().ok()?;
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
            class_replace(
                &mut fragment,
                "network-control",
                network_class,
                NETWORK_GLYPH,
            );
        }
        class_replace(&mut fragment, "input-control", input_class, input_label);
        let powered = state
            .adapters
            .first()
            .is_some_and(|adapter| adapter.powered);
        let discovering = state
            .adapters
            .first()
            .is_some_and(|adapter| adapter.discovering);
        let devices = state.devices.iter().map(|device| {
                let action = if device.connected { "disconnect" } else if device.paired { "connect" } else { "pair" };
                let label = if device.connected { "Disconnect" } else if device.paired { "Connect" } else { "Pair" };
                format!(r#"<article class="connectivity-item bluetooth-device"><span><b>{}</b><small>{}</small></span><button onclick="event.stopPropagation();bluetoothAction('{}','{}',this)">{}</button></article>"#, escape(&device.name), escape(&device.address), escape(&device.address), action, label)
            }).collect::<String>();
        let devices = if devices.is_empty() {
            r#"<p class="connectivity-empty">No devices found</p>"#.to_owned()
        } else {
            devices
        };
        fragment.push_str(&format!(r#"<div id="bluetooth-panel" class="connectivity-panel" hx-swap-oob="outerHTML"><header><h2>Bluetooth</h2><div class="connectivity-actions"><button onclick="bluetoothCommand('power',this)">{}</button><button onclick="bluetoothCommand('scan',this)">{}</button></div></header><main>{devices}</main></div>"#, if powered { "Turn off" } else { "Turn on" }, if discovering { "Stop scan" } else { "Add device" }));
        let mut networks = network.as_ref().map(|state| state.connections.iter().filter(|connection| connection.device == "enp5s0" || connection.kind.contains("ethernet")).map(|connection| format!(r#"<article class="connectivity-item connected"><span><b>{}</b><small>{}</small></span><em>Wired · Connected</em></article>"#, escape(&connection.name), escape(&connection.device))).collect::<String>()).unwrap_or_default();
        if let Ok(status) = NmcliBackend.tailscale_status() {
            networks.push_str(&format!(r#"<article class="connectivity-item tailscale"><span><b>Tailscale</b><small>{}</small></span><em>{}</em></article>"#, escape(&status.dns_name), if status.online { "Connected" } else { "Offline" }));
        }
        if let Ok(wifi) = NmcliBackend.wifi_networks() {
            networks.push_str(&wifi.into_iter().map(|network| format!(r#"<article class="connectivity-item"><span><b>{}</b><small>{}</small></span><em>{}%{}</em></article>"#, escape(&network.ssid), escape(&network.security), network.signal, if network.active { " · Connected" } else { "" })).collect::<String>());
        }
        let networks = if networks.is_empty() {
            r#"<p class="connectivity-empty">No networks</p>"#.to_owned()
        } else {
            networks
        };
        fragment.push_str(&format!(r#"<div id="network-panel" class="connectivity-panel" hx-swap-oob="outerHTML"><header><h2>Wi-Fi</h2><span>Networks</span></header><main>{networks}</main></div>"#));
        Some(fragment)
    })
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn class_replace(html: &mut String, id: &str, class: &str, value: &str) {
    let (action, title) = match id {
        "bluetooth-control" => (BLUETOOTH_DIALOG_ACTION, "Bluetooth"),
        "network-control" => (
            "document.querySelector('#network-dialog').showModal()",
            "Wi-Fi",
        ),
        "input-control" => (
            "fetch('/components/connectivity/input-method/toggle',{method:'POST'})",
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
