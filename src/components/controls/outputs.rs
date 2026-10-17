//! Linux output selection: BlueZ connects devices; PulseAudio/PipeWire routes playback.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, bail};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tokio::{
    process::Command,
    sync::Mutex,
    time::{sleep, timeout},
};

use crate::components::connectivity::bluetooth::{BluetoothBackend, BluetoothDevice};

pub(super) static AUDIO_OPERATION: Mutex<()> = Mutex::const_new(());
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum OutputTarget {
    Sink(String),
    Bluetooth(String),
}

#[derive(Debug, Serialize)]
struct Output {
    target: OutputTarget,
    name: String,
    connected: bool,
    selected: bool,
}

#[derive(Serialize)]
struct OutputState {
    supported: bool,
    outputs: Vec<Output>,
    volume: Option<u16>,
    warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Sink {
    name: String,
    description: String,
    #[serde(default)]
    properties: HashMap<String, serde_json::Value>,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: HashMap<String, ChannelVolume>,
}

#[derive(Clone, Debug, Deserialize)]
struct ChannelVolume {
    value: u32,
}

impl Sink {
    fn matches(&self, device: &BluetoothDevice) -> bool {
        ["api.bluez5.address", "device.string"].iter().any(|key| {
            self.properties
                .get(*key)
                .and_then(|v| v.as_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(&device.address))
        }) || ["api.bluez5.path", "bluez.path"].iter().any(|key| {
            self.properties
                .get(*key)
                .and_then(|v| v.as_str())
                .is_some_and(|value| value == device.path)
        }) || self.name.to_ascii_lowercase().starts_with(&format!(
            "bluez_output.{}.",
            device.address.replace(':', "_").to_ascii_lowercase()
        ))
    }

    fn percent(&self) -> Option<u16> {
        if self.mute {
            return Some(0);
        }
        if self.volume.is_empty() {
            return None;
        }
        let total: u64 = self
            .volume
            .values()
            .map(|channel| u64::from(channel.value))
            .sum();
        Some(((total as f64 / self.volume.len() as f64 / 65536.0 * 100.0).round() as u16).min(100))
    }
}

trait Backend {
    async fn sinks(&self) -> Result<Vec<Sink>>;
    async fn default_sink(&self) -> Result<String>;
    async fn devices(&self) -> Result<Vec<BluetoothDevice>>;
    async fn connect(&self, path: &str) -> Result<()>;
    async fn set_default(&self, name: &str) -> Result<()>;
    async fn streams(&self) -> Result<Vec<u32>>;
    async fn move_stream(&self, index: u32, name: &str) -> Result<()>;
}

struct SystemBackend;

async fn pactl(args: &[&str]) -> Result<String> {
    let output = timeout(
        COMMAND_TIMEOUT,
        Command::new("pactl")
            .args(args)
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("audio command timed out")?
    .context("failed to execute pactl; check that it is installed")?;
    if !output.status.success() {
        bail!(
            "pactl {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("pactl returned non-UTF-8 output")
}

impl Backend for SystemBackend {
    async fn sinks(&self) -> Result<Vec<Sink>> {
        serde_json::from_str(&pactl(&["--format=json", "list", "sinks"]).await?)
            .context("invalid audio output list")
    }
    async fn default_sink(&self) -> Result<String> {
        Ok(pactl(&["get-default-sink"]).await?.trim().to_owned())
    }
    async fn devices(&self) -> Result<Vec<BluetoothDevice>> {
        let state = timeout(COMMAND_TIMEOUT, BluetoothBackend.state_without_batteries())
            .await
            .context("Bluetooth query timed out")??;
        match state.availability {
            crate::components::connectivity::bluetooth::BluetoothAvailability::Available => {
                Ok(state
                    .devices
                    .into_iter()
                    .filter(|d| d.paired && d.audio_output)
                    .collect())
            }
            crate::components::connectivity::bluetooth::BluetoothAvailability::Unavailable {
                reason,
            } => bail!("{reason}"),
        }
    }
    async fn connect(&self, path: &str) -> Result<()> {
        BluetoothBackend.set_device_connected(path, true).await
    }
    async fn set_default(&self, name: &str) -> Result<()> {
        pactl(&["set-default-sink", name]).await.map(|_| ())
    }
    async fn streams(&self) -> Result<Vec<u32>> {
        #[derive(Deserialize)]
        struct Stream {
            index: u32,
        }
        let streams: Vec<Stream> =
            serde_json::from_str(&pactl(&["--format=json", "list", "sink-inputs"]).await?)
                .context("invalid playback stream list")?;
        Ok(streams.into_iter().map(|s| s.index).collect())
    }
    async fn move_stream(&self, index: u32, name: &str) -> Result<()> {
        pactl(&["move-sink-input", &index.to_string(), name])
            .await
            .map(|_| ())
    }
}

fn merge_outputs(sinks: &[Sink], devices: &[BluetoothDevice], default: &str) -> Vec<Output> {
    let mut outputs: Vec<_> = sinks
        .iter()
        .filter(|sink| !devices.iter().any(|device| sink.matches(device)))
        .map(|sink| Output {
            target: OutputTarget::Sink(sink.name.clone()),
            name: if sink.name.starts_with("alsa_output.") && sink.name.contains(".hdmi-") {
                "Speaker".into()
            } else {
                sink.description.clone()
            },
            connected: true,
            selected: sink.name == default,
        })
        .collect();
    outputs.extend(devices.iter().map(|device| {
        Output {
            target: OutputTarget::Bluetooth(device.path.clone()),
            name: device.name.clone(),
            connected: device.connected,
            selected: sinks
                .iter()
                .any(|sink| sink.matches(device) && sink.name == default),
        }
    }));
    outputs
}

async fn state(backend: &impl Backend) -> Result<OutputState> {
    let sinks = backend.sinks().await?;
    let default = backend.default_sink().await?;
    let (devices, warning) = match backend.devices().await {
        Ok(devices) => (devices, None),
        Err(error) => (Vec::new(), Some(format!("Bluetooth unavailable: {error}"))),
    };
    Ok(OutputState {
        supported: true,
        outputs: merge_outputs(&sinks, &devices, &default),
        volume: sinks
            .iter()
            .find(|sink| sink.name == default)
            .and_then(Sink::percent),
        warning,
    })
}

async fn resolve(backend: &impl Backend, target: &OutputTarget, poll: Duration) -> Result<String> {
    match target {
        OutputTarget::Sink(name) => {
            if !backend.sinks().await?.iter().any(|sink| &sink.name == name) {
                bail!("Output is no longer available. Refresh and try again.");
            }
            Ok(name.clone())
        }
        OutputTarget::Bluetooth(path) => {
            let device = backend
                .devices()
                .await?
                .into_iter()
                .find(|device| &device.path == path)
                .context("Paired audio device is no longer available.")?;
            if !device.connected {
                backend.connect(path).await.context("Unable to connect headphones. Check Bluetooth and make sure they are awake and nearby.")?;
            }
            loop {
                let sinks = backend.sinks().await?;
                let default = backend.default_sink().await?;
                // Keep a currently selected profile when the device offers several sinks.
                if let Some(sink) = sinks
                    .iter()
                    .find(|s| s.matches(&device) && s.name == default)
                    .or_else(|| sinks.iter().find(|s| s.matches(&device)))
                {
                    return Ok(sink.name.clone());
                }
                sleep(poll).await;
            }
        }
    }
}

async fn select(
    backend: &impl Backend,
    target: &OutputTarget,
    ready_timeout: Duration,
    poll: Duration,
) -> Result<Option<String>> {
    let name = timeout(ready_timeout, resolve(backend, target, poll)).await
        .context("Audio output did not become ready. Check that the headphones are awake and nearby, then retry.")??;
    // Do not change the default until a real sink exists.
    backend.set_default(&name).await?;
    let migration = timeout(Duration::from_secs(15), async {
        let mut failures = Vec::new();
        for index in backend.streams().await? {
            if let Err(error) = backend.move_stream(index, &name).await {
                // A stream can finish between enumeration and migration.
                if backend.streams().await?.contains(&index) {
                    failures.push(error.to_string());
                }
            }
        }
        if !failures.is_empty() {
            bail!("{}", failures.join("; "));
        }
        anyhow::Ok(())
    })
    .await;
    let warning = match migration {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!(
            "Output changed, but some playback could not be moved: {error}"
        )),
        Err(_) => Some("Output changed, but moving existing playback timed out.".into()),
    };
    if backend.default_sink().await? != name {
        bail!("The default output changed elsewhere. Refresh and try again.");
    }
    Ok(warning)
}

fn failure(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({"error": error.to_string()})),
    )
        .into_response()
}

pub(super) async fn get_outputs() -> Response {
    match state(&SystemBackend).await {
        Ok(state) => Json(state).into_response(),
        Err(error) => failure(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

pub(super) async fn select_output(Json(target): Json<OutputTarget>) -> Response {
    let Ok(_guard) = AUDIO_OPERATION.try_lock() else {
        return failure(
            StatusCode::CONFLICT,
            "Another audio operation is in progress. Please retry.",
        );
    };
    match select(
        &SystemBackend,
        &target,
        Duration::from_secs(30),
        Duration::from_millis(500),
    )
    .await
    {
        Ok(warning) => Json(serde_json::json!({"warning": warning})).into_response(),
        Err(error) => {
            tracing::warn!(%error, "audio output selection failed");
            failure(StatusCode::SERVICE_UNAVAILABLE, error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    fn device() -> BluetoothDevice {
        BluetoothDevice {
            path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF".into(),
            adapter_path: Some("/org/bluez/hci0".into()),
            address: "AA:BB:CC:DD:EE:FF".into(),
            name: "My headphones".into(),
            paired: true,
            connected: false,
            trusted: false,
            audio_output: true,
            battery: None,
        }
    }

    fn speaker() -> Sink {
        serde_json::from_value(serde_json::json!({
            "name": "alsa_output.speaker", "description": "Speakers", "state": "SUSPENDED",
            "volume": {"left": {"value": 32768}, "right": {"value": 32768}}
        }))
        .unwrap()
    }

    fn headphones() -> Sink {
        serde_json::from_value(serde_json::json!({
            "name": "bluez_output.AA_BB_CC_DD_EE_FF.1", "description": "My headphones",
            "properties": {"api.bluez5.address": "aa:bb:cc:dd:ee:ff"},
            "volume": {"left": {"value": 16384}}
        }))
        .unwrap()
    }

    struct Fake {
        ready_after: usize,
        polls: AtomicUsize,
        connected: AtomicBool,
        default: StdMutex<String>,
        events: StdMutex<Vec<String>>,
        connect_fails: bool,
        fail_stream: Option<u32>,
        ended_stream: bool,
    }

    impl Fake {
        fn new(ready_after: usize) -> Self {
            Self {
                ready_after,
                polls: AtomicUsize::new(0),
                connected: AtomicBool::new(false),
                default: StdMutex::new(speaker().name),
                events: StdMutex::new(Vec::new()),
                connect_fails: false,
                fail_stream: None,
                ended_stream: false,
            }
        }
        fn record(&self, event: String) {
            self.events.lock().unwrap().push(event);
        }
    }

    impl Backend for Fake {
        async fn sinks(&self) -> Result<Vec<Sink>> {
            let mut sinks = vec![speaker()];
            if self.connected.load(Ordering::Relaxed)
                && self.polls.fetch_add(1, Ordering::Relaxed) >= self.ready_after
            {
                sinks.push(headphones());
            }
            Ok(sinks)
        }
        async fn default_sink(&self) -> Result<String> {
            Ok(self.default.lock().unwrap().clone())
        }
        async fn devices(&self) -> Result<Vec<BluetoothDevice>> {
            let mut device = device();
            device.connected = self.connected.load(Ordering::Relaxed);
            Ok(vec![device])
        }
        async fn connect(&self, _: &str) -> Result<()> {
            self.record("connect".into());
            if self.connect_fails {
                bail!("out of range");
            }
            self.connected.store(true, Ordering::Relaxed);
            Ok(())
        }
        async fn set_default(&self, name: &str) -> Result<()> {
            self.record(format!("default:{name}"));
            *self.default.lock().unwrap() = name.into();
            Ok(())
        }
        async fn streams(&self) -> Result<Vec<u32>> {
            let has_moved = self
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event.starts_with("move:1:"));
            Ok(if self.ended_stream && has_moved {
                vec![2]
            } else {
                vec![1, 2]
            })
        }
        async fn move_stream(&self, index: u32, name: &str) -> Result<()> {
            self.record(format!("move:{index}:{name}"));
            if self.fail_stream == Some(index) {
                bail!("stream move failed");
            }
            Ok(())
        }
    }

    fn run(future: impl std::future::Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future);
    }

    #[test]
    fn merges_disconnected_devices_without_name_based_identity() {
        let device = device();
        let before = merge_outputs(&[speaker()], std::slice::from_ref(&device), &speaker().name);
        assert_eq!(before.len(), 2);
        assert!(before[0].selected);
        assert!(!before[1].connected);
        let mut connected = device.clone();
        connected.connected = true;
        let mut same_name = device.clone();
        same_name.address = "11:22:33:44:55:66".into();
        same_name.path = "/org/bluez/hci0/dev_11_22_33_44_55_66".into();
        let after = merge_outputs(
            &[speaker(), headphones()],
            &[connected, same_name],
            &headphones().name,
        );
        assert_eq!(after.len(), 3);
        assert_eq!(before[1].target, after[1].target);
        assert!(after[1].selected);
        assert!(!after[2].selected);
        assert_ne!(after[1].target, after[2].target);
    }

    #[test]
    fn matches_metadata_and_fallback_names_without_prefix_collisions() {
        let mut sink = headphones();
        sink.name = "unrelated".into();
        assert!(sink.matches(&device()));
        sink.properties.clear();
        sink.properties
            .insert("bluez.path".into(), device().path.into());
        assert!(sink.matches(&device()));
        sink.properties.clear();
        assert!(!sink.matches(&device()));
        sink.name = headphones().name;
        assert!(sink.matches(&device()));
        sink.name = "bluez_output.AA_BB_CC_DD_EE_FF_00.1".into();
        assert!(!sink.matches(&device()));
        assert_eq!(speaker().percent(), Some(50));
        sink.mute = true;
        assert_eq!(sink.percent(), Some(0));
    }

    #[test]
    fn waits_for_sink_then_changes_default_and_moves_all_streams() {
        run(async {
            let backend = Fake::new(2);
            assert!(
                select(
                    &backend,
                    &OutputTarget::Bluetooth(device().path),
                    Duration::from_secs(1),
                    Duration::from_millis(1)
                )
                .await
                .unwrap()
                .is_none()
            );
            assert_eq!(
                *backend.events.lock().unwrap(),
                vec![
                    "connect".to_owned(),
                    format!("default:{}", headphones().name),
                    format!("move:1:{}", headphones().name),
                    format!("move:2:{}", headphones().name)
                ]
            );
            let current = state(&backend).await.unwrap();
            assert_eq!(current.volume, Some(25));
            assert!(current.outputs[1].selected);
            select(
                &backend,
                &OutputTarget::Sink(speaker().name),
                Duration::from_secs(1),
                Duration::ZERO,
            )
            .await
            .unwrap();
            assert!(backend.connected.load(Ordering::Relaxed));
            assert_eq!(backend.default_sink().await.unwrap(), speaker().name);
        });
    }

    #[test]
    fn timeout_and_connection_failure_preserve_default() {
        run(async {
            for fail in [false, true] {
                let mut backend = Fake::new(usize::MAX);
                backend.connect_fails = fail;
                assert!(
                    select(
                        &backend,
                        &OutputTarget::Bluetooth(device().path),
                        Duration::from_millis(10),
                        Duration::from_millis(1)
                    )
                    .await
                    .is_err()
                );
                assert_eq!(backend.default_sink().await.unwrap(), speaker().name);
                assert_eq!(*backend.events.lock().unwrap(), vec!["connect"]);
            }
        });
    }

    #[test]
    fn does_not_reconnect_connected_device_or_accept_unknown_target() {
        run(async {
            let backend = Fake::new(0);
            backend.connected.store(true, Ordering::Relaxed);
            select(
                &backend,
                &OutputTarget::Bluetooth(device().path),
                Duration::from_secs(1),
                Duration::ZERO,
            )
            .await
            .unwrap();
            assert!(
                !backend
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| event == "connect")
            );
            assert!(
                resolve(
                    &backend,
                    &OutputTarget::Sink("missing".into()),
                    Duration::ZERO
                )
                .await
                .is_err()
            );
            assert!(
                resolve(
                    &backend,
                    &OutputTarget::Bluetooth("missing".into()),
                    Duration::ZERO
                )
                .await
                .is_err()
            );
        });
    }

    #[test]
    fn stream_failures_are_partial_success_but_finished_streams_are_ignored() {
        run(async {
            for ended in [false, true] {
                let mut backend = Fake::new(0);
                backend.fail_stream = Some(1);
                backend.ended_stream = ended;
                let warning = select(
                    &backend,
                    &OutputTarget::Sink(speaker().name),
                    Duration::from_secs(1),
                    Duration::ZERO,
                )
                .await
                .unwrap();
                assert_eq!(warning.is_some(), !ended);
                assert!(
                    backend
                        .events
                        .lock()
                        .unwrap()
                        .contains(&format!("move:2:{}", speaker().name))
                );
            }
        });
    }

    #[test]
    fn concurrent_selection_is_rejected_before_accessing_system_services() {
        run(async {
            let _guard = AUDIO_OPERATION.lock().await;
            let response = select_output(Json(OutputTarget::Sink("test".into()))).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                super::super::set_volume(axum::extract::Path(50)).await,
                StatusCode::CONFLICT
            );
        });
    }
}
