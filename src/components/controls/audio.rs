#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeState {
    pub percent: u16,
    pub muted: bool,
}

pub struct WpctlBackend;

#[cfg(target_os = "linux")]
mod platform {
    use std::process::Command;

    use anyhow::{Context, Result, bail};

    use super::{VolumeState, WpctlBackend};

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
            let stdout =
                String::from_utf8(output.stdout).context("wpctl returned non-UTF-8 output")?;
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
}

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use platform::parse_wpctl_volume;

#[cfg(target_os = "windows")]
mod platform {
    use std::ptr;

    use anyhow::{Context, Result, bail};
    use windows::Win32::{
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia,
            eRender,
        },
        System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
    };

    use super::{VolumeState, WpctlBackend};

    fn endpoint() -> Result<IAudioEndpointVolume> {
        unsafe {
            // RPC_E_CHANGED_MODE is harmless: COM was already initialized on this thread.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("failed to create Core Audio device enumerator")?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .context("no default Core Audio render endpoint")?;
            device
                .Activate(CLSCTX_ALL, None)
                .context("failed to activate endpoint volume")
        }
    }

    impl WpctlBackend {
        pub fn current(&self) -> Result<VolumeState> {
            unsafe {
                let endpoint = endpoint()?;
                let scalar = endpoint
                    .GetMasterVolumeLevelScalar()
                    .context("failed to read endpoint volume")?;
                if !scalar.is_finite() {
                    bail!("Core Audio returned an invalid volume");
                }
                Ok(VolumeState {
                    percent: (scalar.clamp(0.0, 1.0) * 100.0).round() as u16,
                    muted: endpoint
                        .GetMute()
                        .context("failed to read endpoint mute state")?
                        .as_bool(),
                })
            }
        }

        pub fn set_percent(&self, percent: u8) -> Result<()> {
            if percent > 100 {
                bail!("volume must be between 0 and 100");
            }
            unsafe {
                endpoint()?
                    .SetMasterVolumeLevelScalar(f32::from(percent) / 100.0, ptr::null())
                    .context("failed to set endpoint volume")
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
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
