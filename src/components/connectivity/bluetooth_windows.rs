use anyhow::{Context, Result, bail};
use windows::Devices::{
    Bluetooth::{BluetoothConnectionStatus, BluetoothDevice as WinBluetoothDevice},
    Enumeration::DeviceInformation,
    Radios::{Radio, RadioKind, RadioState},
};

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
#[derive(Debug, Default, Clone, Copy)]
pub struct BluetoothBackend;

impl BluetoothBackend {
    pub async fn state(&self) -> Result<BluetoothState> {
        let radios = Radio::GetRadiosAsync()
            .context("enumerate Windows radios")?
            .join()
            .context("enumerate Windows radios")?;
        let mut adapters = Vec::new();
        for radio in &radios {
            if radio.Kind()? == RadioKind::Bluetooth {
                adapters.push(BluetoothAdapter {
                    path: "bluetooth".into(),
                    address: String::new(),
                    alias: radio.Name()?.to_string(),
                    powered: radio.State()? == RadioState::On,
                    discovering: false,
                });
            }
        }
        if adapters.is_empty() {
            return Ok(BluetoothState {
                availability: BluetoothAvailability::Unavailable {
                    reason: "no Bluetooth radio".into(),
                },
                adapters,
                devices: vec![],
            });
        }
        let selector = WinBluetoothDevice::GetDeviceSelector()?;
        let infos = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.join()?;
        let mut devices = Vec::new();
        for info in infos {
            let id = info.Id()?.to_string();
            let Some(device) = WinBluetoothDevice::FromIdAsync(&id.clone().into())?
                .join()
                .ok()
            else {
                continue;
            };
            let address_num = device.BluetoothAddress()?;
            devices.push(BluetoothDevice {
                path: id,
                adapter_path: Some("bluetooth".into()),
                address: format_address(address_num),
                name: info.Name()?.to_string(),
                paired: info.Pairing()?.IsPaired()?,
                connected: device.ConnectionStatus()? == BluetoothConnectionStatus::Connected,
                trusted: info.Pairing()?.IsPaired()?,
                battery: None,
            });
        }
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(BluetoothState {
            availability: BluetoothAvailability::Available,
            adapters,
            devices,
        })
    }
    pub async fn set_adapter_powered(&self, _: &str, powered: bool) -> Result<()> {
        let radios = Radio::GetRadiosAsync()?.join()?;
        for radio in radios {
            if radio.Kind()? == RadioKind::Bluetooth {
                radio
                    .SetStateAsync(if powered {
                        RadioState::On
                    } else {
                        RadioState::Off
                    })?
                    .join()?;
                return Ok(());
            }
        }
        bail!("no Bluetooth radio")
    }
    pub async fn set_discovering(&self, _: &str, _: bool) -> Result<()> {
        bail!("Bluetooth discovery control is unavailable to Windows desktop apps")
    }
    pub async fn pair_device(&self, address: &str) -> Result<()> {
        let state = self.state().await?;
        let item = state
            .devices
            .iter()
            .find(|d| d.address.eq_ignore_ascii_case(address))
            .context("unknown Bluetooth device")?;
        let info = DeviceInformation::CreateFromIdAsync(&item.path.clone().into())?.join()?;
        let result = info.Pairing()?.PairAsync()?.join()?;
        anyhow::ensure!(
            result.Status()? == windows::Devices::Enumeration::DevicePairingResultStatus::Paired
                || result.Status()?
                    == windows::Devices::Enumeration::DevicePairingResultStatus::AlreadyPaired,
            "Windows pairing failed: {:?}",
            result.Status()?
        );
        Ok(())
    }
    pub async fn set_device_connected_by_address(&self, _: &str, _: bool) -> Result<()> {
        bail!(
            "Bluetooth connect/disconnect is managed by Windows and unavailable through the desktop WinRT API"
        )
    }
}

fn format_address(value: u64) -> String {
    let b = value.to_be_bytes();
    b[2..]
        .iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}
