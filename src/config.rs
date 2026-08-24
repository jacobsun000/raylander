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
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ExternalDockConfig {
    pub enabled: bool,
    pub bind_address: IpAddr,
    pub port: u16,
    pub weather_latitude: String,
    pub weather_longitude: String,
    pub google_calendar_urls: Vec<String>,
}

impl Default for ExternalDockConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind_address: "0.0.0.0".parse().expect("valid default address"),
            port: 8_030,
            weather_latitude: "30.2672".into(),
            weather_longitude: "-97.7431".into(),
            google_calendar_urls: Vec::new(),
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
            "#,
        )
        .unwrap();
        assert_eq!(config.components.external_dock.port, 8080);
        assert_eq!(
            config.components.external_dock.bind_address,
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
    }
}
