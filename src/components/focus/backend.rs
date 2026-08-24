use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

pub struct HyprlandBackend;

impl HyprlandBackend {
    pub fn query<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        let output = Command::new("hyprctl")
            .args(["-j", name])
            .output()
            .with_context(|| format!("failed to execute hyprctl -j {name}"))?;
        if !output.status.success() {
            bail!(
                "hyprctl -j {name} exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("hyprctl -j {name} returned invalid JSON"))
    }

    pub fn focus_workspace(&self, id: i32) -> Result<()> {
        let command = format!("hl.dsp.focus({{workspace={id}}})");
        let output = Command::new("hyprctl")
            .args(["dispatch", &command])
            .output()
            .context("failed to execute hyprctl workspace dispatch")?;
        if !output.status.success() {
            bail!(
                "hyprctl workspace dispatch exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}
