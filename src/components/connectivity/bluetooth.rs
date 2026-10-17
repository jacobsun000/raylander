//! BlueZ-backed Bluetooth state and actions.
//!
//! This module deliberately exposes domain values rather than D-Bus values.  Callers can render
//! [`BluetoothAvailability::Unavailable`] as an absent control without treating a machine without
//! BlueZ as an application failure.

use std::{collections::HashMap, fs, process::Command};

use anyhow::{Context, Result, bail};
use zbus::{
    Connection, Proxy,
    fdo::ManagedObjects,
    zvariant::{ObjectPath, OwnedValue},
};

const BLUEZ: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const BATTERY_IFACE: &str = "org.bluez.Battery1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothAvailability {
    Available,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothAdapter {
    pub path: String,
    pub address: String,
    pub alias: String,
    pub powered: bool,
    pub discovering: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub path: String,
    pub adapter_path: Option<String>,
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub trusted: bool,
    pub audio_output: bool,
    pub battery: Option<BluetoothBattery>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BluetoothBattery {
    pub percentage: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothState {
    pub availability: BluetoothAvailability,
    pub adapters: Vec<BluetoothAdapter>,
    pub devices: Vec<BluetoothDevice>,
}

impl BluetoothState {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            availability: BluetoothAvailability::Unavailable {
                reason: reason.into(),
            },
            adapters: Vec::new(),
            devices: Vec::new(),
        }
    }
}

/// A small, stateless BlueZ client. It is cheap to construct and safe to move to a source thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct BluetoothBackend;

impl BluetoothBackend {
    pub async fn state(&self) -> Result<BluetoothState> {
        let mut state = self.state_without_batteries().await?;
        fill_missing_batteries(&mut state.devices);
        Ok(state)
    }

    /// Audio selection does not need the optional, process-backed battery lookup.
    pub async fn state_without_batteries(&self) -> Result<BluetoothState> {
        let connection = match Connection::system().await {
            Ok(connection) => connection,
            Err(error) => {
                return Ok(BluetoothState::unavailable(format!(
                    "system D-Bus unavailable: {error}"
                )));
            }
        };
        let proxy = match Proxy::new(
            &connection,
            BLUEZ,
            "/",
            "org.freedesktop.DBus.ObjectManager",
        )
        .await
        {
            Ok(proxy) => proxy,
            Err(error) => return Ok(BluetoothState::unavailable(error.to_string())),
        };
        let objects: ManagedObjects = match proxy.call("GetManagedObjects", &()).await {
            Ok(objects) => objects,
            Err(error) if bluez_is_absent(&error) => {
                return Ok(BluetoothState::unavailable("BlueZ is not running"));
            }
            Err(error) => return Err(error).context("query BlueZ managed objects"),
        };
        Ok(parse_managed_objects(&objects))
    }

    /// Powers an adapter only when `adapter_path` currently identifies a BlueZ adapter.
    pub async fn set_adapter_powered(&self, adapter_path: &str, powered: bool) -> Result<()> {
        let state = self.state().await?;
        ensure_available(&state)?;
        if !state
            .adapters
            .iter()
            .any(|adapter| adapter.path == adapter_path)
        {
            bail!("unknown Bluetooth adapter path: {adapter_path}");
        }
        let connection = Connection::system()
            .await
            .context("connect to system D-Bus")?;
        let proxy = Proxy::new(&connection, BLUEZ, adapter_path, ADAPTER_IFACE)
            .await
            .context("create BlueZ adapter proxy")?;
        proxy
            .set_property("Powered", powered)
            .await
            .context("set BlueZ adapter power")
    }

    pub async fn set_discovering(&self, adapter_path: &str, discovering: bool) -> Result<()> {
        let state = self.state().await?;
        ensure_available(&state)?;
        if !state
            .adapters
            .iter()
            .any(|adapter| adapter.path == adapter_path)
        {
            bail!("unknown Bluetooth adapter path: {adapter_path}");
        }
        let connection = Connection::system()
            .await
            .context("connect to system D-Bus")?;
        let proxy = Proxy::new(&connection, BLUEZ, adapter_path, ADAPTER_IFACE)
            .await
            .context("create BlueZ adapter proxy")?;
        let method = if discovering {
            "StartDiscovery"
        } else {
            "StopDiscovery"
        };
        proxy
            .call::<_, _, ()>(method, &())
            .await
            .with_context(|| method.to_owned())
    }

    pub async fn pair_device(&self, address: &str) -> Result<()> {
        let state = self.state().await?;
        ensure_available(&state)?;
        let device = state
            .devices
            .iter()
            .find(|device| device.address == address)
            .with_context(|| format!("unknown Bluetooth device: {address}"))?;
        if device.paired {
            return Ok(());
        }
        let connection = Connection::system()
            .await
            .context("connect to system D-Bus")?;
        let proxy = Proxy::new(&connection, BLUEZ, device.path.as_str(), DEVICE_IFACE)
            .await
            .context("create BlueZ device proxy")?;
        proxy
            .call::<_, _, ()>("Pair", &())
            .await
            .context("pair Bluetooth device")
    }

    pub async fn set_device_connected_by_address(
        &self,
        address: &str,
        connected: bool,
    ) -> Result<()> {
        let state = self.state().await?;
        let path = state
            .devices
            .iter()
            .find(|device| device.address == address)
            .with_context(|| format!("unknown Bluetooth device: {address}"))?
            .path
            .clone();
        self.set_device_connected(&path, connected).await
    }

    /// Connects or disconnects a known paired device. Unpaired devices are intentionally rejected.
    pub async fn set_device_connected(&self, device_path: &str, connected: bool) -> Result<()> {
        let state = self.state_without_batteries().await?;
        ensure_available(&state)?;
        let device = state
            .devices
            .iter()
            .find(|device| device.path == device_path)
            .with_context(|| format!("unknown Bluetooth device path: {device_path}"))?;
        if !device.paired {
            bail!("refusing to control unpaired Bluetooth device: {device_path}");
        }
        if device.connected == connected {
            return Ok(());
        }
        let connection = Connection::system()
            .await
            .context("connect to system D-Bus")?;
        let proxy = Proxy::new(&connection, BLUEZ, device_path, DEVICE_IFACE)
            .await
            .context("create BlueZ device proxy")?;
        let method = if connected { "Connect" } else { "Disconnect" };
        proxy
            .call::<_, _, ()>(method, &())
            .await
            .with_context(|| format!("{method} Bluetooth device"))
    }
}

fn is_audio_output_uuid(uuid: &str) -> bool {
    // A2DP Audio Sink, Headset, and Hands-Free (not the audio gateway roles).
    ["0000110b", "00001108", "0000111e"]
        .iter()
        .any(|prefix| uuid.eq_ignore_ascii_case(&format!("{prefix}-0000-1000-8000-00805f9b34fb")))
}

fn ensure_available(state: &BluetoothState) -> Result<()> {
    match &state.availability {
        BluetoothAvailability::Available => Ok(()),
        BluetoothAvailability::Unavailable { reason } => bail!("Bluetooth unavailable: {reason}"),
    }
}

fn bluez_is_absent(error: &zbus::Error) -> bool {
    let text = error.to_string();
    text.contains("ServiceUnknown") || text.contains("NameHasNoOwner")
}

fn parse_managed_objects(objects: &ManagedObjects) -> BluetoothState {
    let mut adapters = Vec::new();
    let mut devices = Vec::new();
    for (path, interfaces) in objects {
        if let Some(properties) = interfaces.get(ADAPTER_IFACE) {
            adapters.push(BluetoothAdapter {
                path: path.to_string(),
                address: string_property(properties, "Address").unwrap_or_default(),
                alias: string_property(properties, "Alias")
                    .or_else(|| string_property(properties, "Name"))
                    .unwrap_or_else(|| "Bluetooth".into()),
                powered: bool_property(properties, "Powered"),
                discovering: bool_property(properties, "Discovering"),
            });
        }
        if let Some(properties) = interfaces.get(DEVICE_IFACE) {
            let battery = interfaces
                .get(BATTERY_IFACE)
                .and_then(|battery| u8_property(battery, "Percentage"))
                .map(|percentage| BluetoothBattery { percentage });
            devices.push(BluetoothDevice {
                path: path.to_string(),
                adapter_path: object_path_property(properties, "Adapter"),
                address: string_property(properties, "Address").unwrap_or_default(),
                name: string_property(properties, "Alias")
                    .or_else(|| string_property(properties, "Name"))
                    .or_else(|| string_property(properties, "Address"))
                    .unwrap_or_else(|| "Unknown device".into()),
                paired: bool_property(properties, "Paired"),
                connected: bool_property(properties, "Connected"),
                trusted: bool_property(properties, "Trusted"),
                audio_output: properties
                    .get("UUIDs")
                    .and_then(|value| <Vec<String>>::try_from(value.try_clone().ok()?).ok())
                    .is_some_and(|uuids| uuids.iter().any(|uuid| is_audio_output_uuid(uuid))),
                battery,
            });
        }
    }
    adapters.sort_by(|a, b| a.path.cmp(&b.path));
    devices.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    BluetoothState {
        availability: BluetoothAvailability::Available,
        adapters,
        devices,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UpowerBattery {
    native_path: String,
    model: String,
    serial: String,
    percentage: u8,
}

fn fill_missing_batteries(devices: &mut [BluetoothDevice]) {
    if devices.iter().all(|device| device.battery.is_some()) {
        return;
    }
    let upower_batteries = query_upower_batteries();
    for device in devices.iter_mut().filter(|device| device.battery.is_none()) {
        let percentage = upower_batteries
            .iter()
            .find(|battery| upower_battery_matches(device, battery))
            .map(|battery| battery.percentage)
            .or_else(|| sysfs_battery_percentage(&device.address));
        if let Some(percentage) = percentage {
            device.battery = Some(BluetoothBattery { percentage });
        }
    }
}

fn query_upower_batteries() -> Vec<UpowerBattery> {
    let Ok(output) = Command::new("upower").arg("--enumerate").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(paths) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty() && !path.ends_with("/DisplayDevice"))
        .filter_map(|path| {
            let output = Command::new("upower")
                .args(["--show-info", path])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let info = String::from_utf8(output.stdout).ok()?;
            parse_upower_battery_info(path, &info)
        })
        .collect()
}

fn parse_upower_battery_info(path: &str, output: &str) -> Option<UpowerBattery> {
    let percentage = upower_property(output, "percentage")?
        .trim_end_matches('%')
        .parse::<f32>()
        .ok()?;
    if !percentage.is_finite() || !(0.0..=100.0).contains(&percentage) {
        return None;
    }
    Some(UpowerBattery {
        native_path: upower_property(output, "native-path")
            .unwrap_or(path)
            .to_owned(),
        model: upower_property(output, "model")
            .unwrap_or_default()
            .to_owned(),
        serial: upower_property(output, "serial")
            .unwrap_or_default()
            .to_owned(),
        percentage: percentage.round() as u8,
    })
}

fn upower_property<'a>(output: &'a str, name: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once(':')?;
        (key == name).then(|| value.trim())
    })
}

fn upower_battery_matches(device: &BluetoothDevice, battery: &UpowerBattery) -> bool {
    let address = compact_identifier(&device.address);
    (!address.is_empty()
        && [battery.serial.as_str(), battery.native_path.as_str()]
            .into_iter()
            .map(compact_identifier)
            .any(|candidate| candidate.contains(&address)))
        || (!battery.model.is_empty() && battery.model.eq_ignore_ascii_case(&device.name))
}

fn sysfs_battery_percentage(address: &str) -> Option<u8> {
    let address = compact_identifier(address);
    if address.is_empty() {
        return None;
    }
    fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !compact_identifier(name).contains(&address)
                || fs::read_to_string(path.join("type")).ok()?.trim() != "Battery"
            {
                return None;
            }
            let percentage = fs::read_to_string(path.join("capacity"))
                .ok()?
                .trim()
                .parse::<u8>()
                .ok()?;
            (percentage <= 100).then_some(percentage)
        })
}

fn compact_identifier(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn string_property(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<&str>().ok())
        .map(str::to_owned)
}

fn bool_property(properties: &HashMap<String, OwnedValue>, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

fn object_path_property(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<ObjectPath<'_>>().ok())
        .map(|path| path.to_string())
}

fn u8_property(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u8> {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<u8>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedObjectPath, Value};

    fn properties(values: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        values
            .iter()
            .map(|(key, value)| ((*key).into(), value.try_clone().unwrap()))
            .collect()
    }

    fn string_value(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(value)).unwrap()
    }

    #[test]
    fn parses_adapter_device_and_battery() {
        let adapter_path = OwnedObjectPath::try_from("/org/bluez/hci0").unwrap();
        let device_path = OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB").unwrap();
        let mut objects = ManagedObjects::new();
        objects.insert(
            adapter_path,
            [(
                ADAPTER_IFACE.try_into().unwrap(),
                properties(&[
                    ("Address", string_value("00:11:22:33:44:55")),
                    ("Alias", string_value("Desktop radio")),
                    ("Powered", OwnedValue::from(true)),
                ]),
            )]
            .into(),
        );
        objects.insert(
            device_path,
            [
                (
                    DEVICE_IFACE.try_into().unwrap(),
                    properties(&[
                        ("Address", string_value("AA:BB:CC:DD:EE:FF")),
                        ("Alias", string_value("Headphones")),
                        (
                            "Adapter",
                            OwnedValue::try_from(Value::from(
                                ObjectPath::try_from("/org/bluez/hci0").unwrap(),
                            ))
                            .unwrap(),
                        ),
                        ("Paired", OwnedValue::from(true)),
                        (
                            "UUIDs",
                            OwnedValue::try_from(Value::from(vec![
                                "0000110b-0000-1000-8000-00805f9b34fb",
                            ]))
                            .unwrap(),
                        ),
                        ("Connected", OwnedValue::from(true)),
                    ]),
                ),
                (
                    BATTERY_IFACE.try_into().unwrap(),
                    properties(&[("Percentage", OwnedValue::from(73_u8))]),
                ),
            ]
            .into(),
        );

        let state = parse_managed_objects(&objects);
        assert_eq!(state.availability, BluetoothAvailability::Available);
        assert_eq!(state.adapters[0].alias, "Desktop radio");
        assert!(state.adapters[0].powered);
        assert_eq!(state.devices[0].name, "Headphones");
        assert!(state.devices[0].paired && state.devices[0].connected);
        assert!(state.devices[0].audio_output);
        assert_eq!(state.devices[0].battery.unwrap().percentage, 73);
    }

    #[test]
    fn audio_services_exclude_hid_and_audio_gateways() {
        for uuid in ["0000110B", "00001108", "0000111e"] {
            assert!(is_audio_output_uuid(&format!(
                "{uuid}-0000-1000-8000-00805f9b34fb"
            )));
        }
        for uuid in ["00001124", "0000110a", "0000111f", "00001112"] {
            assert!(!is_audio_output_uuid(&format!(
                "{uuid}-0000-1000-8000-00805f9b34fb"
            )));
        }
        assert!(!is_audio_output_uuid("AirPods"));
    }

    #[test]
    fn parser_supplies_stable_defaults_for_partial_bluez_objects() {
        let path = OwnedObjectPath::try_from("/org/bluez/hci0/dev_unknown").unwrap();
        let objects = [(
            path,
            [(DEVICE_IFACE.try_into().unwrap(), HashMap::new())].into(),
        )]
        .into();

        let state = parse_managed_objects(&objects);
        assert_eq!(state.devices[0].name, "Unknown device");
        assert!(!state.devices[0].paired);
        assert_eq!(state.devices[0].battery, None);
    }

    #[test]
    fn parses_upower_battery_and_matches_bluetooth_address() {
        let battery = parse_upower_battery_info(
            "/org/freedesktop/UPower/devices/battery_ps_controller",
            "native-path: ps-controller-battery-a0:ab:51:cf:f9:f2\nmodel: DualSense Wireless Controller\nserial: a0:ab:51:cf:f9:f2\npercentage: 95%\n",
        )
        .unwrap();
        assert_eq!(battery.percentage, 95);
        let device = BluetoothDevice {
            path: "/org/bluez/hci0/dev_A0_AB_51_CF_F9_F2".into(),
            adapter_path: None,
            address: "A0:AB:51:CF:F9:F2".into(),
            name: "DualSense Wireless Controller".into(),
            audio_output: false,
            paired: true,
            connected: true,
            trusted: true,
            battery: None,
        };
        assert!(upower_battery_matches(&device, &battery));
    }

    #[test]
    fn rejects_upower_percentages_outside_range() {
        assert!(parse_upower_battery_info("BAT0", "percentage: 101%\n").is_none());
        assert!(parse_upower_battery_info("BAT0", "percentage: -1%\n").is_none());
    }

    #[test]
    fn unavailable_state_is_empty() {
        let state = BluetoothState::unavailable("BlueZ is not running");
        assert!(matches!(
            state.availability,
            BluetoothAvailability::Unavailable { .. }
        ));
        assert!(state.adapters.is_empty() && state.devices.is_empty());
    }
}
