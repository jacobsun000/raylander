use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

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
        let connections = run_nmcli(&[
            "--terse",
            "--escape",
            "yes",
            "--fields",
            "TYPE,NAME,DEVICE",
            "connection",
            "show",
            "--active",
        ])?;
        let radio = run_nmcli(&["--terse", "radio", "wifi"])?;
        let wifi = run_nmcli(&[
            "--terse",
            "--escape",
            "yes",
            "--fields",
            "IN-USE,SIGNAL",
            "device",
            "wifi",
            "list",
            "--rescan",
            "no",
        ])?;
        Ok(NetworkState {
            connections: parse_active_connections(&connections)?,
            wifi_enabled: parse_wifi_radio(&radio)?,
            wifi_signal: parse_active_wifi_signal(&wifi)?,
        })
    }

    pub fn wifi_networks(&self) -> Result<Vec<WifiNetwork>> {
        let output = run_nmcli(&[
            "--terse",
            "--escape",
            "yes",
            "--fields",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "--rescan",
            "yes",
        ])?;
        parse_wifi_networks(&output)
    }

    pub fn tailscale_status(&self) -> Result<TailscaleStatus> {
        let output = Command::new("tailscale")
            .args(["status", "--json"])
            .output()
            .context("failed to execute tailscale status --json")?;
        if !output.status.success() {
            bail!(
                "tailscale status exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let wire: TailscaleWire = serde_json::from_slice(&output.stdout)
            .context("tailscale returned invalid status JSON")?;
        Ok(TailscaleStatus {
            online: wire.own.online,
            dns_name: wire.own.dns_name.trim_end_matches('.').to_owned(),
        })
    }
}

fn run_nmcli(args: &[&str]) -> Result<String> {
    let output = Command::new("nmcli")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute nmcli {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "nmcli {} exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("nmcli returned non-UTF-8 output")
}

pub fn parse_active_connections(output: &str) -> Result<Vec<ActiveConnection>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = split_escaped(line, ':')?;
            if fields.len() != 3 {
                bail!("expected three fields in nmcli active connection output");
            }
            Ok(ActiveConnection {
                kind: fields[0].clone(),
                name: fields[1].clone(),
                device: fields[2].clone(),
            })
        })
        .collect()
}

pub fn parse_wifi_radio(output: &str) -> Result<bool> {
    match output.trim() {
        "enabled" => Ok(true),
        "disabled" => Ok(false),
        value => bail!("unexpected nmcli Wi-Fi radio state: {value}"),
    }
}

pub fn parse_wifi_networks(output: &str) -> Result<Vec<WifiNetwork>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = match split_escaped(line, ':') {
                Ok(fields) => fields,
                Err(error) => return Some(Err(error)),
            };
            if fields.len() != 4 {
                return Some(Err(anyhow::anyhow!(
                    "expected four fields in nmcli Wi-Fi list output"
                )));
            }
            if fields[1].is_empty() {
                return None;
            }
            let signal = match fields[2].parse::<u8>() {
                Ok(signal) if signal <= 100 => signal,
                _ => return Some(Err(anyhow::anyhow!("invalid Wi-Fi signal"))),
            };
            Some(Ok(WifiNetwork {
                active: fields[0] == "*" || fields[0] == "yes",
                ssid: fields[1].clone(),
                signal,
                security: fields[3].clone(),
            }))
        })
        .collect()
}

pub fn parse_active_wifi_signal(output: &str) -> Result<Option<u8>> {
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let fields = split_escaped(line, ':')?;
        if fields.len() != 2 {
            bail!("expected two fields in nmcli Wi-Fi output");
        }
        if fields[0] == "*" || fields[0] == "yes" {
            let signal = fields[1]
                .parse::<u8>()
                .context("nmcli returned an invalid Wi-Fi signal")?;
            if signal > 100 {
                bail!("nmcli returned Wi-Fi signal above 100");
            }
            return Ok(Some(signal));
        }
    }
    Ok(None)
}

fn split_escaped(input: &str, separator: char) -> Result<Vec<String>> {
    let mut fields = vec![String::new()];
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            fields.last_mut().unwrap().push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == separator {
            fields.push(String::new());
        } else {
            fields.last_mut().unwrap().push(character);
        }
    }
    if escaped {
        bail!("unterminated escape in nmcli output");
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_and_escaped_connections() {
        assert!(parse_active_connections("").unwrap().is_empty());
        assert_eq!(
            parse_active_connections(
                "802-11-wireless:Cafe\\: upstairs:wlan0\nvpn:Work\\\\VPN:tun0\n"
            )
            .unwrap(),
            vec![
                ActiveConnection {
                    kind: "802-11-wireless".into(),
                    name: "Cafe: upstairs".into(),
                    device: "wlan0".into()
                },
                ActiveConnection {
                    kind: "vpn".into(),
                    name: "Work\\VPN".into(),
                    device: "tun0".into()
                },
            ]
        );
    }

    #[test]
    fn rejects_malformed_connection_output() {
        assert!(parse_active_connections("wifi:missing-device").is_err());
        assert!(parse_active_connections("wifi:name:device\\").is_err());
    }

    #[test]
    fn parses_wifi_network_list() {
        assert_eq!(
            parse_wifi_networks("*:Cafe\\: Upstairs:87:WPA2\n:Guest:42:\n").unwrap(),
            vec![
                WifiNetwork {
                    active: true,
                    ssid: "Cafe: Upstairs".into(),
                    signal: 87,
                    security: "WPA2".into(),
                },
                WifiNetwork {
                    active: false,
                    ssid: "Guest".into(),
                    signal: 42,
                    security: String::new(),
                },
            ]
        );
    }

    #[test]
    fn rejects_invalid_wifi_network_rows() {
        assert!(parse_wifi_networks("*:missing:fields").is_err());
        assert!(parse_wifi_networks("*:Cafe:101:WPA2").is_err());
    }

    #[test]
    fn ignores_hidden_wifi_networks() {
        assert!(parse_wifi_networks("::50:WPA2").unwrap().is_empty());
    }

    #[test]
    fn parses_radio_and_active_signal() {
        assert!(parse_wifi_radio("enabled\n").unwrap());
        assert!(!parse_wifi_radio("disabled").unwrap());
        assert!(parse_wifi_radio("unknown").is_err());
        assert_eq!(parse_active_wifi_signal("no:60\n*:87\n").unwrap(), Some(87));
        assert_eq!(parse_active_wifi_signal("").unwrap(), None);
        assert!(parse_active_wifi_signal("*:101").is_err());
    }
}
