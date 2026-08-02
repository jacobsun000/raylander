use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub theme: PathBuf,
    pub components: ComponentsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ComponentsConfig {
    pub volume_osd: VolumeOsdConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct VolumeOsdConfig {
    pub enabled: bool,
    pub monitor: String,
    pub position: Position,
    pub margin: i32,
    pub timeout_ms: u64,
    pub fade_ms: u32,
    pub step_percent: u16,
    pub limit_percent: u16,
    pub observe_external: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    TopCenter,
    #[default]
    BottomCenter,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: PathBuf::from("theme.css"),
            components: ComponentsConfig::default(),
        }
    }
}

impl Default for VolumeOsdConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor: "DP-2".into(),
            position: Position::BottomCenter,
            margin: 64,
            timeout_ms: 1_400,
            fade_ms: 160,
            step_percent: 1,
            limit_percent: 100,
            observe_external: true,
        }
    }
}

pub fn config_dir() -> PathBuf {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("raylander");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/raylander")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }

    let source =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let config: Config =
        toml::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))?;
    validate(&config)?;
    Ok(config)
}

pub fn theme_path(config: &Config) -> PathBuf {
    if config.theme.is_absolute() {
        config.theme.clone()
    } else {
        config_dir().join(&config.theme)
    }
}

fn validate(config: &Config) -> Result<()> {
    let volume = &config.components.volume_osd;
    anyhow::ensure!(
        volume.step_percent > 0,
        "step-percent must be greater than zero"
    );
    anyhow::ensure!(
        volume.limit_percent >= volume.step_percent,
        "limit-percent must be at least step-percent"
    );
    anyhow::ensure!(
        volume.timeout_ms > 0,
        "timeout-ms must be greater than zero"
    );
    anyhow::ensure!(volume.margin >= 0, "margin must not be negative");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_desktop() {
        let config = Config::default();
        assert_eq!(config.components.volume_osd.monitor, "DP-2");
        assert_eq!(config.components.volume_osd.step_percent, 1);
        assert!(config.components.volume_osd.observe_external);
    }

    #[test]
    fn parses_partial_config_with_defaults() {
        let config: Config = toml::from_str(
            r#"
                [components.volume-osd]
                monitor = "HDMI-A-1"
                timeout-ms = 900
            "#,
        )
        .unwrap();
        assert_eq!(config.components.volume_osd.monitor, "HDMI-A-1");
        assert_eq!(config.components.volume_osd.timeout_ms, 900);
        assert_eq!(config.components.volume_osd.step_percent, 1);
    }
}
