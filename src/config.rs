use std::{env, fs, net::IpAddr, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub theme: PathBuf,
    pub color_scheme: ColorScheme,
    pub reduce_transparency: bool,
    pub components: ComponentsConfig,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ComponentsConfig {
    pub volume_osd: VolumeOsdConfig,
    pub menu_bar: MenuBarConfig,
    pub sidebar: SidebarConfig,
    pub desktop: DesktopConfig,
    pub notifications: NotificationsConfig,
    pub external_dock: ExternalDockConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct MenuBarConfig {
    pub enabled: bool,
    pub monitors: Vec<String>,
    pub left: Vec<BarItem>,
    pub center: Vec<BarItem>,
    pub right: Vec<BarItem>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct SidebarConfig {
    pub enabled: bool,
    pub monitor: MonitorSelector,
    pub sections: Vec<SidebarSection>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DesktopConfig {
    pub enabled: bool,
    pub widgets: Vec<DesktopWidgetConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct NotificationsConfig {
    pub enabled: bool,
    pub history_limit: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ExternalDockConfig {
    pub enabled: bool,
    pub bind_address: IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DesktopWidgetConfig {
    pub component: ComponentId,
    pub monitor: String,
    pub presentation: GridSize,
    pub column: u8,
    pub row: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BarItem {
    SystemMenu,
    ActiveApp,
    Workspaces,
    Playing,
    Packages,
    InputMethod,
    Tray,
    Audio,
    Network,
    Notifications,
    Clock,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarSection {
    QuickSettings,
    Playing,
    Calendar,
    Packages,
    Notifications,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentId {
    Playing,
    Clock,
    Calendar,
    Notifications,
    Audio,
    Network,
    Packages,
    InputMethod,
    SystemHealth,
    Storage,
    Display,
    Power,
    Privacy,
    Bluetooth,
    Weather,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum GridSize {
    #[serde(rename = "1x1")]
    OneByOne,
    #[serde(rename = "1x2")]
    OneByTwo,
    #[serde(rename = "2x1")]
    TwoByOne,
    #[serde(rename = "2x2")]
    TwoByTwo,
}

impl GridSize {
    pub fn columns(self) -> u8 {
        match self {
            Self::OneByOne | Self::OneByTwo => 1,
            Self::TwoByOne | Self::TwoByTwo => 2,
        }
    }

    pub fn rows(self) -> u8 {
        match self {
            Self::OneByOne | Self::TwoByOne => 1,
            Self::OneByTwo | Self::TwoByTwo => 2,
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::OneByOne => "size-1x1",
            Self::OneByTwo => "size-1x2",
            Self::TwoByOne => "size-2x1",
            Self::TwoByTwo => "size-2x2",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum MonitorSelector {
    Named(String),
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
            color_scheme: ColorScheme::System,
            reduce_transparency: false,
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

impl Default for MenuBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            monitors: vec!["*".into()],
            left: vec![BarItem::SystemMenu, BarItem::ActiveApp],
            center: vec![BarItem::Workspaces],
            right: vec![
                BarItem::Playing,
                BarItem::Packages,
                BarItem::InputMethod,
                BarItem::Tray,
                BarItem::Audio,
                BarItem::Network,
                BarItem::Notifications,
                BarItem::Clock,
            ],
        }
    }
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor: MonitorSelector::Named("focused".into()),
            sections: vec![
                SidebarSection::QuickSettings,
                SidebarSection::Playing,
                SidebarSection::Calendar,
                SidebarSection::Packages,
                SidebarSection::Notifications,
            ],
        }
    }
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            widgets: vec![
                DesktopWidgetConfig::new(ComponentId::Playing, GridSize::TwoByOne, 0, 0),
                DesktopWidgetConfig::new(ComponentId::Clock, GridSize::TwoByOne, 2, 0),
                DesktopWidgetConfig::new(ComponentId::SystemHealth, GridSize::TwoByOne, 0, 1),
                DesktopWidgetConfig::new(ComponentId::Calendar, GridSize::TwoByTwo, 2, 1),
            ],
        }
    }
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            history_limit: 100,
        }
    }
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

impl Default for DesktopWidgetConfig {
    fn default() -> Self {
        Self::new(ComponentId::Clock, GridSize::OneByOne, 0, 0)
    }
}

impl DesktopWidgetConfig {
    fn new(component: ComponentId, presentation: GridSize, column: u8, row: u8) -> Self {
        Self {
            component,
            monitor: "DP-2".into(),
            presentation,
            column,
            row,
        }
    }

    pub(crate) fn demo(
        component: ComponentId,
        presentation: GridSize,
        column: u8,
        row: u8,
    ) -> Self {
        Self::new(component, presentation, column, row)
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
    anyhow::ensure!(
        !config.components.menu_bar.monitors.is_empty(),
        "menu-bar monitors must not be empty"
    );
    anyhow::ensure!(
        config.components.notifications.history_limit <= 1_000,
        "notification history-limit must not exceed 1000"
    );
    anyhow::ensure!(
        config.components.external_dock.port > 0,
        "external-dock port must be greater than zero"
    );
    validate_desktop_layout(&config.components.desktop.widgets)?;
    Ok(())
}

fn validate_desktop_layout(widgets: &[DesktopWidgetConfig]) -> Result<()> {
    for (index, widget) in widgets.iter().enumerate() {
        let right = u16::from(widget.column) + u16::from(widget.presentation.columns());
        let bottom = u16::from(widget.row) + u16::from(widget.presentation.rows());
        anyhow::ensure!(
            right <= 32 && bottom <= 32,
            "desktop widget grid exceeds 32x32"
        );
        for other in &widgets[..index] {
            if widget.monitor != other.monitor {
                continue;
            }
            let overlaps_columns = widget.column < other.column + other.presentation.columns()
                && other.column < widget.column + widget.presentation.columns();
            let overlaps_rows = widget.row < other.row + other.presentation.rows()
                && other.row < widget.row + widget.presentation.rows();
            anyhow::ensure!(
                !(overlaps_columns && overlaps_rows),
                "desktop widgets overlap on monitor {}",
                widget.monitor
            );
        }
    }
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
        assert!(config.components.external_dock.enabled);
        assert_eq!(config.components.external_dock.port, 8_030);
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
        assert_eq!(config.color_scheme, ColorScheme::System);
    }

    #[test]
    fn parses_external_dock_config() {
        let config: Config = toml::from_str(
            r#"
                [components.external-dock]
                enabled = true
                bind-address = "127.0.0.1"
                port = 8080
            "#,
        )
        .unwrap();
        assert_eq!(
            config.components.external_dock.bind_address,
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(config.components.external_dock.port, 8080);
    }

    #[test]
    fn parses_atlas_color_scheme() {
        let config: Config = toml::from_str("color-scheme = \"light\"").unwrap();
        assert_eq!(config.color_scheme, ColorScheme::Light);
    }

    #[test]
    fn parses_desktop_widget_grid_sizes() {
        let config: Config = toml::from_str(
            r#"
                [components.desktop]
                [[components.desktop.widgets]]
                component = "playing"
                monitor = "DP-3"
                presentation = "1x2"
                column = 3
                row = 4
            "#,
        )
        .unwrap();
        let widget = &config.components.desktop.widgets[0];
        assert_eq!(widget.presentation, GridSize::OneByTwo);
        assert_eq!(
            (widget.presentation.columns(), widget.presentation.rows()),
            (1, 2)
        );
    }

    #[test]
    fn rejects_overlapping_desktop_widgets() {
        let mut config = Config::default();
        config.components.desktop.widgets = vec![
            DesktopWidgetConfig::new(ComponentId::Clock, GridSize::TwoByTwo, 0, 0),
            DesktopWidgetConfig::new(ComponentId::Playing, GridSize::OneByOne, 1, 1),
        ];
        assert!(validate(&config).is_err());
    }
}
