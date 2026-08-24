use std::{env, fs, net::IpAddr, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub components: ComponentsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ComponentsConfig {
    pub external_dock: ExternalDockConfig,
    pub dock_layout: DockLayoutConfig,
    pub clock: ComponentToggle,
    pub controls: ComponentToggle,
    pub connectivity: ComponentToggle,
    pub packages: ComponentToggle,
    pub focus: ComponentToggle,
    pub media: ComponentToggle,
    pub system_status: ComponentToggle,
    pub weather: WeatherConfig,
    pub agents: ComponentToggle,
    pub calendar_events: CalendarEventsConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ComponentToggle {
    pub enabled: bool,
}

impl Default for ComponentToggle {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct WeatherConfig {
    pub enabled: bool,
    pub latitude: String,
    pub longitude: String,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            latitude: "30.2672".into(),
            longitude: "-97.7431".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct CalendarEventsConfig {
    pub enabled: bool,
    pub urls: Vec<String>,
}

impl Default for CalendarEventsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            urls: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DockLayoutConfig {
    pub columns: u16,
    pub order: Vec<String>,
}

impl Default for DockLayoutConfig {
    fn default() -> Self {
        Self {
            columns: 2,
            order: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ExternalDockConfig {
    pub enabled: bool,
    pub bind_address: IpAddr,
    pub port: u16,
}

impl Default for ExternalDockConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind_address: "0.0.0.0".parse().expect("valid default address"),
            port: 8_030,
        }
    }
}

pub fn config_path() -> PathBuf {
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    root.join("raylander/config.toml")
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
    anyhow::ensure!(
        config.components.external_dock.port > 0,
        "external-dock port must be greater than zero"
    );
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_external_dock_config() {
        let config: Config = toml::from_str(
            r#"
                [components.external-dock]
                bind-address = "127.0.0.1"
                port = 8080

                [components.dock-layout]
                columns = 3
                order = ["clock", "weather"]

                [components.weather]
                latitude = "44.9778"
                longitude = "-93.2650"
            "#,
        )
        .unwrap();
        assert_eq!(config.components.external_dock.port, 8080);
        assert_eq!(
            config.components.external_dock.bind_address,
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(config.components.dock_layout.columns, 3);
        assert_eq!(config.components.weather.latitude, "44.9778");
    }
}
