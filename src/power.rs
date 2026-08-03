use std::{fs, io::ErrorKind, path::Path, process::Command};

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryStatus {
    Charging,
    Discharging,
    Full,
    Empty,
    PendingCharge,
    PendingDischarge,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BatteryState {
    pub device: String,
    pub percentage: f32,
    pub status: BatteryStatus,
    pub time_to_empty_seconds: Option<u64>,
    pub time_to_full_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerState {
    pub battery: Option<BatteryState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    Lock,
    Logout,
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
}

impl SessionAction {
    pub fn requires_confirmation(self) -> bool {
        !matches!(self, Self::Lock)
    }

    fn loginctl_args(self) -> Result<Vec<String>> {
        match self {
            Self::Lock => Ok(vec!["lock-session".into()]),
            Self::Logout => Ok(vec!["terminate-user".into(), current_user_id()?]),
            Self::Suspend => Ok(vec!["suspend".into()]),
            Self::Hibernate => Ok(vec!["hibernate".into()]),
            Self::Reboot => Ok(vec!["reboot".into()]),
            Self::PowerOff => Ok(vec!["poweroff".into()]),
        }
    }
}

pub struct PowerBackend;

impl PowerBackend {
    pub fn current(&self) -> Result<PowerState> {
        match query_upower() {
            Ok(state) => Ok(state),
            Err(error) if is_command_missing(&error) => {
                query_sysfs(Path::new("/sys/class/power_supply"))
            }
            Err(error) => Err(error),
        }
    }

    pub fn apply_session_action(&self, action: SessionAction, confirmed: bool) -> Result<()> {
        if action.requires_confirmation() && !confirmed {
            bail!("session action {action:?} requires confirmation");
        }
        let args = action.loginctl_args()?;
        let status = Command::new("loginctl")
            .args(&args)
            .status()
            .with_context(|| format!("failed to execute loginctl {}", args.join(" ")))?;
        if !status.success() {
            bail!("loginctl {} exited with {status}", args.join(" "));
        }
        Ok(())
    }
}

fn current_user_id() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to determine the current user ID")?;
    if !output.status.success() {
        bail!("id -u exited with {}", output.status);
    }
    let id = String::from_utf8(output.stdout).context("id -u returned non-UTF-8 output")?;
    let id = id.trim();
    id.parse::<u32>()
        .context("id -u returned an invalid user ID")?;
    Ok(id.to_owned())
}

fn query_upower() -> Result<PowerState> {
    let devices = run_upower(&["--enumerate"])?;
    let Some(device) = devices.lines().find(|line| line.contains("/battery_")) else {
        return Ok(PowerState::default());
    };
    let info = run_upower(&["--show-info", device.trim()])?;
    Ok(PowerState {
        battery: Some(parse_upower_battery(device.trim(), &info)?),
    })
}

fn run_upower(args: &[&str]) -> Result<String> {
    let output = Command::new("upower")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute upower {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("upower {} exited with {}", args.join(" "), output.status);
    }
    String::from_utf8(output.stdout).context("upower returned non-UTF-8 output")
}

fn is_command_missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::NotFound)
    })
}

pub fn parse_upower_battery(device: &str, output: &str) -> Result<BatteryState> {
    let percentage = required_property(output, "percentage")?
        .trim_end_matches('%')
        .parse::<f32>()
        .context("UPower returned an invalid battery percentage")?;
    if !percentage.is_finite() || !(0.0..=100.0).contains(&percentage) {
        bail!("UPower returned battery percentage outside 0..=100");
    }
    Ok(BatteryState {
        device: device.to_owned(),
        percentage,
        status: parse_status(required_property(output, "state")?),
        time_to_empty_seconds: property(output, "time to empty").and_then(parse_duration),
        time_to_full_seconds: property(output, "time to full").and_then(parse_duration),
    })
}

fn property<'a>(output: &'a str, name: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once(':')?;
        (key == name).then(|| value.trim())
    })
}

fn required_property<'a>(output: &'a str, name: &str) -> Result<&'a str> {
    property(output, name).with_context(|| format!("UPower output omitted {name}"))
}

fn parse_status(value: &str) -> BatteryStatus {
    match value {
        "charging" => BatteryStatus::Charging,
        "discharging" => BatteryStatus::Discharging,
        "fully-charged" => BatteryStatus::Full,
        "empty" => BatteryStatus::Empty,
        "pending-charge" => BatteryStatus::PendingCharge,
        "pending-discharge" => BatteryStatus::PendingDischarge,
        _ => BatteryStatus::Unknown,
    }
}

fn parse_duration(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let amount = fields.next()?.parse::<f64>().ok()?;
    let multiplier = match fields.next()? {
        "seconds" | "second" => 1.0,
        "minutes" | "minute" => 60.0,
        "hours" | "hour" => 3600.0,
        _ => return None,
    };
    (amount.is_finite() && amount >= 0.0).then(|| (amount * multiplier).round() as u64)
}

fn query_sysfs(root: &Path) -> Result<PowerState> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(PowerState::default()),
        Err(error) => return Err(error).context("failed to read power supplies from sysfs"),
    };
    for entry in entries {
        let path = entry?.path();
        if read_trimmed(&path.join("type")).as_deref() != Some("Battery") {
            continue;
        }
        let percentage = read_trimmed(&path.join("capacity"))
            .context("battery capacity is unavailable")?
            .parse::<f32>()
            .context("sysfs returned invalid battery capacity")?;
        let status = read_trimmed(&path.join("status"))
            .map(|value| parse_status(&value.to_ascii_lowercase()))
            .unwrap_or(BatteryStatus::Unknown);
        return Ok(PowerState {
            battery: Some(BatteryState {
                device: path.to_string_lossy().into_owned(),
                percentage,
                status,
                time_to_empty_seconds: None,
                time_to_full_seconds: None,
            }),
        });
    }
    Ok(PowerState::default())
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upower_battery() {
        let state = parse_upower_battery(
            "/org/freedesktop/UPower/devices/battery_BAT0",
            "  state: discharging\n  time to empty: 2.5 hours\n  percentage: 73%\n",
        )
        .unwrap();
        assert_eq!(state.percentage, 73.0);
        assert_eq!(state.status, BatteryStatus::Discharging);
        assert_eq!(state.time_to_empty_seconds, Some(9000));
        assert_eq!(state.time_to_full_seconds, None);
    }

    #[test]
    fn rejects_invalid_percentage() {
        assert!(parse_upower_battery("BAT0", "state: charging\npercentage: 101%\n").is_err());
    }

    #[test]
    fn destructive_actions_require_confirmation() {
        assert!(!SessionAction::Lock.requires_confirmation());
        assert!(SessionAction::Logout.requires_confirmation());
        assert!(SessionAction::Suspend.requires_confirmation());
        assert!(SessionAction::PowerOff.requires_confirmation());
    }
}
