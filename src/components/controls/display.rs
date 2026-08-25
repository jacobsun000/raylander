#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
use anyhow::{Context, Result, bail};
#[cfg(target_os = "linux")]
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdcDisplay {
    pub number: u32,
    pub connector: Option<String>,
    pub bus: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brightness {
    pub current: u16,
    pub maximum: u16,
}

impl Brightness {
    pub fn percent(self) -> u8 {
        if self.maximum == 0 {
            return 0;
        }
        ((u32::from(self.current) * 100) / u32::from(self.maximum)).min(100) as u8
    }
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprMonitor {
    dpms_status: bool,
}

pub struct DdcutilBackend;

#[cfg(target_os = "linux")]
impl DdcutilBackend {
    pub fn display_power_enabled(&self) -> Result<bool> {
        let output = Command::new("hyprctl")
            .args(["-j", "monitors"])
            .output()
            .context("failed to execute hyprctl -j monitors")?;
        if !output.status.success() {
            bail!(
                "hyprctl monitors exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let monitors: Vec<HyprMonitor> = serde_json::from_slice(&output.stdout)
            .context("hyprctl monitors returned invalid JSON")?;
        Ok(monitors.iter().any(|monitor| monitor.dpms_status))
    }

    pub fn displays(&self) -> Result<Vec<DdcDisplay>> {
        let output = run_ddcutil(&["detect", "--brief"])?;
        parse_ddcutil_detect(&output)
    }

    pub fn brightness(&self, display: u32) -> Result<Brightness> {
        let display = display.to_string();
        let output = run_ddcutil(&["getvcp", "10", "--brief", "--display", &display])?;
        parse_ddcutil_brightness(&output)
    }

    pub fn all_brightness(&self) -> Result<Vec<(DdcDisplay, Brightness)>> {
        self.displays()?
            .into_iter()
            .map(|display| {
                self.brightness(display.number)
                    .map(|value| (display, value))
            })
            .collect()
    }

    pub fn set_brightness(&self, display: u32, percent: u8) -> Result<()> {
        if percent > 100 {
            bail!("brightness must be between 0 and 100");
        }
        let display = display.to_string();
        let value = percent.to_string();
        run_ddcutil(&["setvcp", "10", &value, "--display", &display])?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod windows_backend {
    use anyhow::{Context, Result, bail};
    use windows::Win32::{
        Devices::Display::{
            DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
        },
        Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromWindow},
    };

    use super::{Brightness, DdcDisplay, DdcutilBackend};

    struct Monitors(Vec<PHYSICAL_MONITOR>);

    impl Drop for Monitors {
        fn drop(&mut self) {
            if !self.0.is_empty() {
                unsafe {
                    let _ = DestroyPhysicalMonitors(&self.0);
                }
            }
        }
    }

    fn monitors() -> Result<Monitors> {
        unsafe {
            let monitor = MonitorFromWindow(Default::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut count = 0;
            GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count)
                .context("display does not expose physical monitor controls")?;
            if count == 0 {
                bail!("display does not expose a physical monitor");
            }
            let mut values = vec![PHYSICAL_MONITOR::default(); count as usize];
            GetPhysicalMonitorsFromHMONITOR(monitor, &mut values)
                .context("failed to open physical monitors")?;
            Ok(Monitors(values))
        }
    }

    impl DdcutilBackend {
        pub fn display_power_enabled(&self) -> Result<bool> {
            // Windows has no reliable, non-disruptive equivalent to Hyprland DPMS state.
            monitors().map(|values| !values.0.is_empty())
        }

        pub fn displays(&self) -> Result<Vec<DdcDisplay>> {
            let values = monitors()?;
            Ok(values
                .0
                .iter()
                .enumerate()
                .map(|(index, monitor)| DdcDisplay {
                    number: index as u32,
                    connector: None,
                    bus: None,
                    model: Some({
                        let description = monitor.szPhysicalMonitorDescription;
                        String::from_utf16_lossy(&description)
                            .trim_end_matches('\0')
                            .to_owned()
                    }),
                })
                .collect())
        }

        pub fn brightness(&self, display: u32) -> Result<Brightness> {
            let values = monitors()?;
            let monitor = values
                .0
                .get(display as usize)
                .context("physical monitor no longer exists")?;
            let (mut minimum, mut current, mut maximum) = (0, 0, 0);
            if unsafe {
                GetMonitorBrightness(
                    monitor.hPhysicalMonitor,
                    &mut minimum,
                    &mut current,
                    &mut maximum,
                )
            } == 0
            {
                return Err(windows::core::Error::from_thread())
                    .context("monitor does not support brightness control");
            }
            let range = maximum.saturating_sub(minimum);
            if range == 0 {
                bail!("monitor returned an invalid brightness range");
            }
            Ok(Brightness {
                current: current.saturating_sub(minimum).min(u32::from(u16::MAX)) as u16,
                maximum: range.min(u32::from(u16::MAX)) as u16,
            })
        }

        pub fn all_brightness(&self) -> Result<Vec<(DdcDisplay, Brightness)>> {
            Ok(self
                .displays()?
                .into_iter()
                .filter_map(|display| {
                    self.brightness(display.number)
                        .ok()
                        .map(|value| (display, value))
                })
                .collect())
        }

        pub fn set_brightness(&self, display: u32, percent: u8) -> Result<()> {
            if percent > 100 {
                bail!("brightness must be between 0 and 100");
            }
            let values = monitors()?;
            let monitor = values
                .0
                .get(display as usize)
                .context("physical monitor no longer exists")?;
            let (mut minimum, mut current, mut maximum) = (0, 0, 0);
            if unsafe {
                GetMonitorBrightness(
                    monitor.hPhysicalMonitor,
                    &mut minimum,
                    &mut current,
                    &mut maximum,
                )
            } == 0
            {
                return Err(windows::core::Error::from_thread())
                    .context("monitor does not support brightness control");
            }
            let value = minimum + maximum.saturating_sub(minimum) * u32::from(percent) / 100;
            if unsafe { SetMonitorBrightness(monitor.hPhysicalMonitor, value) } == 0 {
                return Err(windows::core::Error::from_thread())
                    .context("failed to set monitor brightness");
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn run_ddcutil(args: &[&str]) -> Result<String> {
    let output = Command::new("ddcutil")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute ddcutil {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "ddcutil {} exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("ddcutil returned non-UTF-8 output")
}

#[cfg(target_os = "linux")]
pub fn parse_ddcutil_detect(output: &str) -> Result<Vec<DdcDisplay>> {
    let mut displays = Vec::new();
    let mut current: Option<DdcDisplay> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(number) = trimmed.strip_prefix("Display ") {
            if let Some(display) = current.take() {
                displays.push(display);
            }
            current = Some(DdcDisplay {
                number: number
                    .trim_end_matches(':')
                    .parse()
                    .context("ddcutil returned an invalid display number")?,
                connector: None,
                bus: None,
                model: None,
            });
            continue;
        }
        let Some(display) = current.as_mut() else {
            continue;
        };
        if let Some(value) = field(trimmed, "DRM connector") {
            display.connector = Some(value.to_owned());
        } else if let Some(value) = field(trimmed, "I2C bus") {
            display.bus = Some(value.to_owned());
        } else if let Some(value) = field(trimmed, "Model") {
            display.model = Some(value.to_owned());
        } else if let Some(value) = field(trimmed, "Monitor") {
            display.model.get_or_insert_with(|| value.to_owned());
        }
    }
    if let Some(display) = current {
        displays.push(display);
    }
    Ok(displays)
}

#[cfg(target_os = "linux")]
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix(name)?.strip_prefix(':').map(str::trim)
}

#[cfg(target_os = "linux")]
pub fn parse_ddcutil_brightness(output: &str) -> Result<Brightness> {
    // Current ddcutil brief output is `VCP 10 C <current> <maximum>`.
    let fields: Vec<_> = output.split_whitespace().collect();
    let marker = fields
        .windows(3)
        .position(|window| window[0].eq_ignore_ascii_case("VCP") && window[1] == "10")
        .context("ddcutil output did not contain VCP code 10")?;
    let values = &fields[marker + 3..];
    let current = values
        .first()
        .context("ddcutil output omitted current brightness")?
        .parse::<u16>()
        .context("ddcutil returned invalid current brightness")?;
    let maximum = values
        .get(1)
        .context("ddcutil output omitted maximum brightness")?
        .parse::<u16>()
        .context("ddcutil returned invalid maximum brightness")?;
    if maximum == 0 || current > maximum {
        bail!("ddcutil returned an invalid brightness range");
    }
    Ok(Brightness { current, maximum })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parses_detected_displays() {
        let output = "Display 1\n   I2C bus:  /dev/i2c-7\n   DRM connector: card1-DP-1\n   Monitor: DEL U2723QE\n\nDisplay 2:\n   I2C bus: /dev/i2c-8\n   Model: LG HDR 4K\n";
        assert_eq!(
            parse_ddcutil_detect(output).unwrap(),
            vec![
                DdcDisplay {
                    number: 1,
                    connector: Some("card1-DP-1".into()),
                    bus: Some("/dev/i2c-7".into()),
                    model: Some("DEL U2723QE".into())
                },
                DdcDisplay {
                    number: 2,
                    connector: None,
                    bus: Some("/dev/i2c-8".into()),
                    model: Some("LG HDR 4K".into())
                },
            ]
        );
    }

    #[test]
    fn parses_brightness_and_rejects_bad_ranges() {
        assert_eq!(
            parse_ddcutil_brightness("VCP 10 C 75 100\n").unwrap(),
            Brightness {
                current: 75,
                maximum: 100
            }
        );
        assert!(parse_ddcutil_brightness("VCP 10 C 101 100").is_err());
        assert!(parse_ddcutil_brightness("VCP 12 C 20 100").is_err());
    }

    #[test]
    fn computes_normalized_percentage() {
        assert_eq!(
            Brightness {
                current: 40,
                maximum: 80
            }
            .percent(),
            50
        );
    }
}
