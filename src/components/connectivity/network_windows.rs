use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;
use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkState {
    pub connections: Vec<ActiveConnection>,
    pub wifi_enabled: bool,
    pub wifi_signal: Option<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveConnection {
    pub kind: String,
    pub name: String,
    pub device: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub active: bool,
    pub ssid: String,
    pub signal: u8,
    pub security: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TailscaleStatus {
    pub online: bool,
    pub dns_name: String,
}
#[derive(Deserialize)]
struct TailscaleWire {
    #[serde(rename = "Self")]
    own: TailscaleSelfWire,
}
#[derive(Deserialize)]
struct TailscaleSelfWire {
    #[serde(rename = "Online")]
    online: bool,
    #[serde(rename = "DNSName")]
    dns_name: String,
}

pub struct NmcliBackend;
impl NmcliBackend {
    pub fn current(&self) -> Result<NetworkState> {
        let profiles = NetworkInformation::GetConnectionProfiles()
            .context("get Windows connection profiles")?;
        let mut connections = Vec::new();
        let mut wifi_enabled = false;
        for profile in profiles {
            let level = profile.GetNetworkConnectivityLevel()?;
            if level == NetworkConnectivityLevel::None {
                continue;
            }
            let name = profile.ProfileName()?.to_string();
            let adapter = profile.NetworkAdapter()?;
            let kind = if profile.IsWlanConnectionProfile()? {
                wifi_enabled = true;
                "wifi"
            } else if profile.IsWwanConnectionProfile()? {
                "mobile"
            } else {
                "ethernet"
            };
            connections.push(ActiveConnection {
                kind: kind.into(),
                name,
                device: format!("{:032x}", adapter.NetworkAdapterId()?.to_u128()),
            });
        }
        Ok(NetworkState {
            connections,
            wifi_enabled,
            wifi_signal: None,
        })
    }

    // WinRT Connectivity exposes known/active profiles, not access-point scanning to unpackaged desktop apps.
    pub fn wifi_networks(&self) -> Result<Vec<WifiNetwork>> {
        let current = self.current()?;
        Ok(current
            .connections
            .into_iter()
            .filter(|c| c.kind == "wifi")
            .map(|c| WifiNetwork {
                active: true,
                ssid: c.name,
                signal: 100,
                security: "Windows managed".into(),
            })
            .collect())
    }

    pub fn tailscale_status(&self) -> Result<TailscaleStatus> {
        let output = Command::new("tailscale")
            .args(["status", "--json"])
            .output()
            .context("execute tailscale")?;
        anyhow::ensure!(output.status.success(), "tailscale status failed");
        let wire: TailscaleWire = serde_json::from_slice(&output.stdout)?;
        Ok(TailscaleStatus {
            online: wire.own.online,
            dns_name: wire.own.dns_name.trim_end_matches('.').into(),
        })
    }
}
