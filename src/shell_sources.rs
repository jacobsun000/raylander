use std::{
    collections::HashMap,
    fs,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    audio::WpctlBackend,
    bluetooth::{BluetoothAvailability, BluetoothBackend},
    display::DdcutilBackend,
    input_method::Fcitx5Backend,
    media::PlayerctlBackend,
    network::NmcliBackend,
    packages::PackageBackend,
    power::PowerBackend,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellUpdate {
    ActiveApp(String),
    Media(String),
    Network(String),
    Packages(usize),
    InputMethod(String),
    Microphone(String),
    Workspaces(HashMap<String, String>),
    SystemHealth(String),
    Storage(String),
    Display(String),
    Power(String),
    Privacy(String),
    Bluetooth(String),
}

pub struct ShellObserver {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    package_refresh: Sender<()>,
}

impl ShellObserver {
    pub fn start() -> (Self, Receiver<ShellUpdate>) {
        let (sender, receiver) = mpsc::channel();
        let (package_refresh, package_requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let threads = vec![
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(1),
                query_active_app,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(1),
                query_workspaces,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(1),
                query_media,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(5),
                query_network,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(1),
                query_input_method,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(2),
                query_microphone,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(2),
                query_system_health,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(60),
                query_storage,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(60),
                query_power,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(300),
                query_display,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(2),
                query_privacy,
            ),
            spawn_source(
                Arc::clone(&stop),
                sender.clone(),
                Duration::from_secs(30),
                query_bluetooth,
            ),
            spawn_package_source(Arc::clone(&stop), sender, package_requests),
        ];
        (
            Self {
                stop,
                threads,
                package_refresh,
            },
            receiver,
        )
    }

    pub fn refresh_packages(&self) {
        let _ = self.package_refresh.send(());
    }
}

fn spawn_package_source(
    stop: Arc<AtomicBool>,
    sender: Sender<ShellUpdate>,
    requests: Receiver<()>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match query_packages() {
                Ok(update) => {
                    if sender.send(update).is_err() {
                        return;
                    }
                }
                Err(error) => tracing::debug!(%error, "package status source unavailable"),
            }
            match requests.recv_timeout(Duration::from_secs(6 * 60 * 60)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    })
}

impl Drop for ShellObserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.package_refresh.send(());
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn spawn_source(
    stop: Arc<AtomicBool>,
    sender: Sender<ShellUpdate>,
    interval: Duration,
    query: fn() -> Result<ShellUpdate>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match query() {
                Ok(update) => {
                    if sender.send(update).is_err() {
                        return;
                    }
                }
                Err(error) => tracing::debug!(%error, "desktop status source unavailable"),
            }
            sleep_interruptibly(&stop, interval);
        }
    })
}

fn sleep_interruptibly(stop: &AtomicBool, duration: Duration) {
    let mut remaining = duration;
    let quantum = Duration::from_millis(100);
    while !remaining.is_zero() && !stop.load(Ordering::Relaxed) {
        let step = remaining.min(quantum);
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
}

#[derive(Deserialize)]
struct HyprlandWindow {
    class: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprlandMonitor {
    name: String,
    active_workspace: HyprlandWorkspace,
}

#[derive(Deserialize)]
struct HyprlandWorkspace {
    id: i32,
    name: String,
}

fn query_active_app() -> Result<ShellUpdate> {
    let output = Command::new("hyprctl")
        .args(["-j", "activewindow"])
        .output()
        .context("failed to execute hyprctl activewindow")?;
    if !output.status.success() {
        bail!("hyprctl activewindow exited with {}", output.status);
    }
    let window: HyprlandWindow =
        serde_json::from_slice(&output.stdout).context("hyprctl returned invalid window JSON")?;
    let value = if window.title.trim().is_empty() {
        window.class
    } else {
        window.title
    };
    Ok(ShellUpdate::ActiveApp(value))
}

fn query_workspaces() -> Result<ShellUpdate> {
    let output = Command::new("hyprctl")
        .args(["-j", "monitors"])
        .output()
        .context("failed to execute hyprctl monitors")?;
    if !output.status.success() {
        bail!("hyprctl monitors exited with {}", output.status);
    }
    let monitors: Vec<HyprlandMonitor> =
        serde_json::from_slice(&output.stdout).context("hyprctl returned invalid monitor JSON")?;
    Ok(ShellUpdate::Workspaces(
        monitors
            .into_iter()
            .map(|monitor| {
                let name = if monitor.active_workspace.name.is_empty() {
                    monitor.active_workspace.id.to_string()
                } else {
                    monitor.active_workspace.name
                };
                (monitor.name, format!("Workspace {name}"))
            })
            .collect(),
    ))
}

fn query_media() -> Result<ShellUpdate> {
    let current = PlayerctlBackend.current()?;
    let label = current
        .map(|state| {
            let artists = state.artists.join(", ");
            if artists.is_empty() {
                state.title
            } else {
                format!("{} — {artists}", state.title)
            }
        })
        .unwrap_or_else(|| "Nothing Playing".into());
    Ok(ShellUpdate::Media(label))
}

fn query_network() -> Result<ShellUpdate> {
    let state = NmcliBackend.current()?;
    let label = state
        .connections
        .iter()
        .find(|connection| connection.kind.contains("wireless"))
        .or_else(|| state.connections.first())
        .map(|connection| connection.name.clone())
        .unwrap_or_else(|| {
            if state.wifi_enabled {
                "Disconnected".into()
            } else {
                "Wi-Fi Off".into()
            }
        });
    Ok(ShellUpdate::Network(label))
}

fn query_packages() -> Result<ShellUpdate> {
    Ok(ShellUpdate::Packages(
        PackageBackend.current()?.total_unique(),
    ))
}

fn query_input_method() -> Result<ShellUpdate> {
    let state = Fcitx5Backend.current()?;
    let label = state
        .name
        .as_deref()
        .map(input_method_label)
        .unwrap_or("—")
        .to_owned();
    Ok(ShellUpdate::InputMethod(label))
}

fn query_microphone() -> Result<ShellUpdate> {
    let state = WpctlBackend.current_microphone()?;
    Ok(ShellUpdate::Microphone(if state.muted {
        "Mic Off".into()
    } else {
        format!("Mic {}%", state.percent)
    }))
}

fn query_system_health() -> Result<ShellUpdate> {
    let load = fs::read_to_string("/proc/loadavg").context("failed to read /proc/loadavg")?;
    let one_minute = load
        .split_whitespace()
        .next()
        .context("loadavg was empty")?;
    let memory = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
    let (total, available) = parse_memory_info(&memory)?;
    let used_percent = if total == 0 {
        0
    } else {
        ((total - available) * 100 / total).min(100)
    };
    Ok(ShellUpdate::SystemHealth(format!(
        "Load {one_minute}\nMemory {used_percent}%"
    )))
}

fn query_storage() -> Result<ShellUpdate> {
    let output = Command::new("df")
        .args(["-Pk", "/"])
        .output()
        .context("failed to execute df")?;
    if !output.status.success() {
        bail!("df exited with {}", output.status);
    }
    let stdout = String::from_utf8(output.stdout).context("df returned non-UTF-8 output")?;
    let percent = parse_storage_percent(&stdout)?;
    Ok(ShellUpdate::Storage(format!("{percent}% used")))
}

fn parse_storage_percent(input: &str) -> Result<u8> {
    let value = input
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(4))
        .context("df output omitted usage percentage")?
        .trim_end_matches('%')
        .parse::<u8>()
        .context("df returned invalid usage percentage")?;
    anyhow::ensure!(value <= 100, "df returned usage above 100%");
    Ok(value)
}

fn query_power() -> Result<ShellUpdate> {
    let state = PowerBackend.current()?;
    let Some(battery) = state.battery else {
        return Ok(ShellUpdate::Power(String::new()));
    };
    Ok(ShellUpdate::Power(format!("{:.0}%", battery.percentage)))
}

fn query_display() -> Result<ShellUpdate> {
    let backend = DdcutilBackend;
    let Some(display) = backend.displays()?.into_iter().next() else {
        return Ok(ShellUpdate::Display(String::new()));
    };
    Ok(ShellUpdate::Display(format!(
        "{}%",
        backend.brightness(display.number)?.percent()
    )))
}

fn query_privacy() -> Result<ShellUpdate> {
    let output = Command::new("pactl")
        .args(["list", "short", "source-outputs"])
        .output()
        .context("failed to query microphone clients")?;
    if !output.status.success() {
        bail!("pactl source-outputs exited with {}", output.status);
    }
    let active = !output.stdout.iter().all(u8::is_ascii_whitespace);
    Ok(ShellUpdate::Privacy(if active {
        "Microphone in use".into()
    } else {
        "Private".into()
    }))
}

fn query_bluetooth() -> Result<ShellUpdate> {
    let state = zbus::block_on(BluetoothBackend.state())?;
    let label = match state.availability {
        BluetoothAvailability::Unavailable { .. } => String::new(),
        BluetoothAvailability::Available => {
            let connected: Vec<_> = state
                .devices
                .iter()
                .filter(|device| device.connected)
                .map(|device| device.name.as_str())
                .collect();
            if !connected.is_empty() {
                connected.join(", ")
            } else if state.adapters.iter().any(|adapter| adapter.powered) {
                "On".into()
            } else {
                "Off".into()
            }
        }
    };
    Ok(ShellUpdate::Bluetooth(label))
}

fn parse_memory_info(input: &str) -> Result<(u64, u64)> {
    let value = |name: &str| -> Result<u64> {
        input
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|line| line.split_whitespace().next())
            .context(format!("meminfo omitted {name}"))?
            .parse()
            .with_context(|| format!("meminfo contained invalid {name}"))
    };
    Ok((value("MemTotal:")?, value("MemAvailable:")?))
}

fn input_method_label(name: &str) -> &str {
    match name {
        "keyboard-us-colemak" | "keyboard-us" => "EN",
        "pinyin" => "中",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_current_machine_input_methods() {
        assert_eq!(input_method_label("keyboard-us-colemak"), "EN");
        assert_eq!(input_method_label("pinyin"), "中");
        assert_eq!(input_method_label("custom"), "custom");
    }

    #[test]
    fn parses_memory_totals() {
        assert_eq!(
            parse_memory_info("MemTotal:       16000 kB\nMemAvailable:    4000 kB\n").unwrap(),
            (16_000, 4_000)
        );
        assert!(parse_memory_info("MemTotal: 100 kB\n").is_err());
    }

    #[test]
    fn parses_storage_percentage() {
        assert_eq!(
            parse_storage_percent("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 100 42 58 42% /\n").unwrap(),
            42
        );
        assert!(parse_storage_percent("bad").is_err());
    }
}
