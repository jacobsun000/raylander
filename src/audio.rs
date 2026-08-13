use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::cli::{MicCommand, VolumeCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeState {
    pub percent: u16,
    pub muted: bool,
}

pub struct WpctlBackend;

impl WpctlBackend {
    pub fn apply(&self, command: VolumeCommand, step: u16, limit: u16) -> Result<VolumeState> {
        match command {
            VolumeCommand::Up => self.run_set(&[
                "set-volume",
                "-l",
                &format!("{:.2}", f64::from(limit) / 100.0),
                "@DEFAULT_AUDIO_SINK@",
                &format!("{step}%+"),
            ])?,
            VolumeCommand::Down => {
                self.run_set(&["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{step}%-")])?
            }
            VolumeCommand::Mute => self.run_set(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])?,
            VolumeCommand::Show => {}
        }
        self.current()
    }

    pub fn current(&self) -> Result<VolumeState> {
        self.current_target("@DEFAULT_AUDIO_SINK@")
    }

    pub fn apply_microphone(
        &self,
        command: MicCommand,
        step: u16,
        limit: u16,
    ) -> Result<VolumeState> {
        match command {
            MicCommand::Up => self.run_set(&[
                "set-volume",
                "-l",
                &format!("{:.2}", f64::from(limit) / 100.0),
                "@DEFAULT_AUDIO_SOURCE@",
                &format!("{step}%+"),
            ])?,
            MicCommand::Down => {
                self.run_set(&["set-volume", "@DEFAULT_AUDIO_SOURCE@", &format!("{step}%-")])?
            }
            MicCommand::Mute => self.run_set(&["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])?,
            MicCommand::Show => {}
        }
        self.current_target("@DEFAULT_AUDIO_SOURCE@")
    }

    pub fn current_microphone(&self) -> Result<VolumeState> {
        self.current_target("@DEFAULT_AUDIO_SOURCE@")
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

    fn run_set(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("wpctl")
            .args(args)
            .status()
            .with_context(|| format!("failed to execute wpctl {}", args.join(" ")))?;
        if !status.success() {
            bail!("wpctl {} exited with {status}", args.join(" "));
        }
        Ok(())
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
