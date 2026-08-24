use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeState {
    pub percent: u16,
    pub muted: bool,
}

pub struct WpctlBackend;

impl WpctlBackend {
    pub fn current(&self) -> Result<VolumeState> {
        self.current_target("@DEFAULT_AUDIO_SINK@")
    }

    pub fn set_percent(&self, percent: u8) -> Result<()> {
        let output = Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{percent}%")])
            .output()
            .context("failed to execute wpctl set-volume")?;
        if !output.status.success() {
            bail!(
                "wpctl set-volume exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn current_target(&self, target: &str) -> Result<VolumeState> {
        let output = Command::new("wpctl")
            .args(["get-volume", target])
            .output()
            .context("failed to execute wpctl get-volume")?;
        if !output.status.success() {
            bail!("wpctl get-volume exited with {}", output.status);
        }
        let stdout = String::from_utf8(output.stdout).context("wpctl returned non-UTF-8 output")?;
        parse_wpctl_volume(&stdout)
    }
}

pub fn parse_wpctl_volume(output: &str) -> Result<VolumeState> {
    let value = output
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())
        .context("wpctl output did not contain a volume value")?;
    if !value.is_finite() || value < 0.0 {
        bail!("wpctl returned an invalid volume value");
    }
    Ok(VolumeState {
        percent: (value * 100.0).round().clamp(0.0, f64::from(u16::MAX)) as u16,
        muted: output.contains("[MUTED]"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_and_muted_volume() {
        assert_eq!(
            parse_wpctl_volume("Volume: 0.36\n").unwrap(),
            VolumeState {
                percent: 36,
                muted: false
            }
        );
        assert_eq!(
            parse_wpctl_volume("Volume: 1.25 [MUTED]\n").unwrap(),
            VolumeState {
                percent: 125,
                muted: true
            }
        );
    }
}
