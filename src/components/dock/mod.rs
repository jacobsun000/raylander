//! Components rendered by the external dock.

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

use anyhow::Result;

use crate::config::ComponentsConfig;

use super::{
    ComponentId, ComponentRegistry, Layout, calendar_events, clock, connectivity, controls, focus,
    media, system_status, weather,
};
#[cfg(target_os = "linux")]
use super::{agents, packages};

pub(crate) mod server;

#[cfg(target_os = "linux")]
const ORDER: [ComponentId; 10] = [
    ComponentId::new("clock"),
    ComponentId::new("controls"),
    ComponentId::new("connectivity"),
    ComponentId::new("packages"),
    ComponentId::new("focus"),
    ComponentId::new("media"),
    ComponentId::new("system-status"),
    ComponentId::new("weather"),
    ComponentId::new("agents"),
    ComponentId::new("calendar-events"),
];

#[cfg(target_os = "windows")]
const ORDER: [ComponentId; 8] = [
    ComponentId::new("clock"),
    ComponentId::new("controls"),
    ComponentId::new("connectivity"),
    ComponentId::new("focus"),
    ComponentId::new("media"),
    ComponentId::new("system-status"),
    ComponentId::new("weather"),
    ComponentId::new("calendar-events"),
];

struct DockSettings {
    columns: u16,
    order: Vec<ComponentId>,
    enabled: HashSet<ComponentId>,
}

static SETTINGS: OnceLock<DockSettings> = OnceLock::new();

pub(crate) fn configure(config: &ComponentsConfig) -> Result<()> {
    let settings = build_settings(config)?;
    let _ = SETTINGS.set(settings);
    Ok(())
}

fn build_settings(config: &ComponentsConfig) -> Result<DockSettings> {
    let known = ORDER
        .iter()
        .map(|id| (id.as_str(), *id))
        .collect::<std::collections::HashMap<_, _>>();
    let explicit_order = !config.dock_layout.order.is_empty();
    let order = if !explicit_order {
        ORDER.to_vec()
    } else {
        let mut seen = HashSet::new();
        config
            .dock_layout
            .order
            .iter()
            .map(|name| {
                let id = *known
                    .get(name.as_str())
                    .ok_or_else(|| anyhow::anyhow!("layout references unknown component {name}"))?;
                anyhow::ensure!(
                    seen.insert(id),
                    "component {name} occurs more than once in layout"
                );
                Ok(id)
            })
            .collect::<Result<Vec<_>>>()?
    };
    anyhow::ensure!(
        config.dock_layout.columns > 0,
        "dock layout columns must be greater than zero"
    );
    let enabled: HashSet<_> = ORDER
        .into_iter()
        .filter(|id| component_enabled(*id, config))
        .collect();
    if explicit_order && let Some(disabled) = order.iter().find(|id| !enabled.contains(id)) {
        anyhow::bail!("layout references disabled component {}", disabled.as_str());
    }
    Ok(DockSettings {
        columns: config.dock_layout.columns,
        order,
        enabled,
    })
}

fn component_enabled(id: ComponentId, config: &ComponentsConfig) -> bool {
    match id.as_str() {
        "clock" => config.clock.enabled,
        "controls" => config.controls.enabled,
        "connectivity" => config.connectivity.enabled,
        #[cfg(target_os = "linux")]
        "packages" => config.packages.enabled,
        "focus" => config.focus.enabled,
        "media" => config.media.enabled,
        "system-status" => config.system_status.enabled,
        "weather" => config.weather.enabled,
        #[cfg(target_os = "linux")]
        "agents" => config.agents.enabled,
        "calendar-events" => config.calendar_events.enabled,
        _ => false,
    }
}

fn settings() -> DockSettings {
    SETTINGS.get().map_or_else(
        || DockSettings {
            columns: 2,
            order: ORDER.to_vec(),
            enabled: ORDER.into_iter().collect(),
        },
        |settings| DockSettings {
            columns: settings.columns,
            order: settings.order.clone(),
            enabled: settings.enabled.clone(),
        },
    )
}

pub struct DockComposition {
    pub dashboard: String,
    pub overlays: String,
}

/// Builds the dock's component set. This is the only place that controls
/// dashboard ordering; component implementations expose only their size.
pub fn registry() -> Result<ComponentRegistry> {
    let settings = settings();
    let mut registry = ComponentRegistry::new();
    macro_rules! register {
        ($id:expr, $component:expr) => {
            if settings.enabled.contains(&$id) {
                registry.register(Arc::new($component))?;
            }
        };
    }
    register!(ComponentId::new("clock"), clock::Clock);
    register!(ComponentId::new("controls"), controls::Controls);
    register!(ComponentId::new("connectivity"), connectivity::Connectivity);
    #[cfg(target_os = "linux")]
    register!(ComponentId::new("packages"), packages::Packages);
    register!(ComponentId::new("focus"), focus::Focus);
    register!(ComponentId::new("media"), media::Media);
    register!(
        ComponentId::new("system-status"),
        system_status::SystemStatus
    );
    register!(ComponentId::new("weather"), weather::Weather);
    #[cfg(target_os = "linux")]
    register!(ComponentId::new("agents"), agents::Agents);
    register!(
        ComponentId::new("calendar-events"),
        calendar_events::CalendarEvents
    );
    Ok(registry)
}

pub(crate) fn update_snapshot() -> String {
    let mut html = controls::update_fragment();
    html.push_str(&connectivity::update_fragment());
    html.push_str(&media::update_fragment());
    #[cfg(target_os = "linux")]
    html.push_str(&packages::update_fragment());
    html.push_str(&focus::oob_fragment());
    html.push_str(&weather::update_fragment());
    #[cfg(target_os = "linux")]
    html.push_str(&agents::update_fragment());
    html.push_str(&calendar_events::update_fragment());
    html.push_str(&media::spectrum_fragment());
    html.push_str(&system_status::update_fragment());
    html
}

pub fn compose() -> Result<DockComposition> {
    let settings = settings();
    let registry = registry()?;
    let order = settings
        .order
        .into_iter()
        .filter(|id| settings.enabled.contains(id))
        .collect();
    let layout = Layout::new(settings.columns, order)?;
    let placements = layout.arrange(registry.components())?;
    let mut dashboard = String::new();
    let mut overlays = String::new();
    for placement in placements {
        let component = registry
            .components()
            .iter()
            .find(|component| component.id() == placement.component)
            .expect("layout placements originate from the component registry");
        let view = component.render()?;
        dashboard.push_str(&view.body);
        overlays.push_str(&view.overlays);
    }
    Ok(DockComposition {
        dashboard,
        overlays,
    })
}

macro_rules! dock_component {
    ($type:ident, $id:literal, $size:expr) => {
        dock_component!($type, $id, $size, axum::Router::new());
    };
    ($type:ident, $id:literal, $size:expr, $routes:expr) => {
        pub struct $type;

        impl crate::components::Component for $type {
            fn id(&self) -> crate::components::ComponentId {
                crate::components::ComponentId::new($id)
            }

            fn size(&self) -> crate::components::GridSize {
                $size
            }

            fn render(&self) -> anyhow::Result<crate::components::ComponentView> {
                Ok(crate::components::ComponentView {
                    body: include_str!("view.html").to_owned(),
                    overlays: include_str!("overlays.html").to_owned(),
                })
            }

            fn routes(&self) -> axum::Router {
                $routes
            }
        }
    };
}

pub(crate) use dock_component;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_and_duplicate_configured_layout_entries() {
        let mut config = ComponentsConfig::default();
        config.dock_layout.order = vec!["clock".into(), "missing".into()];
        assert!(build_settings(&config).is_err());
        config.dock_layout.order = vec!["clock".into(), "clock".into()];
        assert!(build_settings(&config).is_err());
        config.dock_layout.order = vec!["weather".into()];
        config.weather.enabled = false;
        assert!(build_settings(&config).is_err());
    }

    #[test]
    fn component_toggles_control_registration_settings() {
        let mut config = ComponentsConfig::default();
        config.weather.enabled = false;
        let settings = build_settings(&config).unwrap();
        assert!(!settings.enabled.contains(&ComponentId::new("weather")));
        assert!(settings.enabled.contains(&ComponentId::new("clock")));
    }

    #[test]
    fn rejects_zero_layout_columns() {
        let mut config = ComponentsConfig::default();
        config.dock_layout.columns = 0;
        assert!(build_settings(&config).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_default_order_contains_all_components() {
        assert_eq!(
            ORDER.map(ComponentId::as_str),
            [
                "clock",
                "controls",
                "connectivity",
                "packages",
                "focus",
                "media",
                "system-status",
                "weather",
                "agents",
                "calendar-events",
            ]
        );
        assert_eq!(
            registry()
                .unwrap()
                .components()
                .iter()
                .map(|component| component.id().as_str())
                .collect::<Vec<_>>(),
            ORDER.map(ComponentId::as_str)
        );
        let dock = compose().unwrap();
        assert!(dock.dashboard.contains(r#"id="packages-card""#));
        assert!(dock.dashboard.contains(r#"id="agent-card""#));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_default_order_contains_only_supported_components() {
        assert_eq!(
            ORDER.map(ComponentId::as_str),
            [
                "clock",
                "controls",
                "connectivity",
                "focus",
                "media",
                "system-status",
                "weather",
                "calendar-events",
            ]
        );
        let settings = build_settings(&ComponentsConfig::default()).unwrap();
        assert!(!settings.enabled.contains(&ComponentId::new("packages")));
        assert!(!settings.order.contains(&ComponentId::new("packages")));
        let registry = registry().unwrap();
        assert_eq!(
            registry
                .components()
                .iter()
                .map(|component| component.id().as_str())
                .collect::<Vec<_>>(),
            ORDER.map(ComponentId::as_str)
        );
        assert!(
            !registry
                .components()
                .iter()
                .any(|component| component.id() == ComponentId::new("packages"))
        );
        let dock = compose().unwrap();
        assert!(!dock.dashboard.contains(r#"id="packages-card""#));
        assert!(!dock.dashboard.contains(r#"id="agent-card""#));
    }

    #[test]
    fn composes_platform_dock_components() {
        let dock = compose().unwrap();
        for element in [
            "clock-card",
            "controls-card",
            "bluetooth-card",
            "focus-card",
            "media-card",
            "system-card",
            "weather-card",
            "calendar-events-card",
        ] {
            assert!(dock.dashboard.contains(&format!(r#"id="{element}""#)));
        }
        assert!(dock.overlays.contains("clock-dialog"));
        assert!(dock.overlays.contains("media-dialog"));
    }
}
