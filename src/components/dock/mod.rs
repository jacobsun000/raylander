//! Components rendered by the external dock.

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

use anyhow::Result;

use crate::config::ComponentsConfig;

use super::{
    ComponentId, ComponentRegistry, Layout, agents, calendar_events, clock, connectivity, controls,
    focus, media, packages, system_status, weather,
};

pub(crate) mod server;

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
    let toggles = [
        (ORDER[0], config.clock.enabled),
        (ORDER[1], config.controls.enabled),
        (ORDER[2], config.connectivity.enabled),
        (ORDER[3], config.packages.enabled),
        (ORDER[4], config.focus.enabled),
        (ORDER[5], config.media.enabled),
        (ORDER[6], config.system_status.enabled),
        (ORDER[7], config.weather.enabled),
        (ORDER[8], config.agents.enabled),
        (ORDER[9], config.calendar_events.enabled),
    ];
    let enabled: HashSet<_> = toggles
        .into_iter()
        .filter_map(|(id, enabled)| enabled.then_some(id))
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
    register!(ORDER[0], clock::Clock);
    register!(ORDER[1], controls::Controls);
    register!(ORDER[2], connectivity::Connectivity);
    register!(ORDER[3], packages::Packages);
    register!(ORDER[4], focus::Focus);
    register!(ORDER[5], media::Media);
    register!(ORDER[6], system_status::SystemStatus);
    register!(ORDER[7], weather::Weather);
    register!(ORDER[8], agents::Agents);
    register!(ORDER[9], calendar_events::CalendarEvents);
    Ok(registry)
}

pub(crate) fn update_snapshot() -> String {
    let mut html = controls::update_fragment();
    html.push_str(&connectivity::update_fragment());
    html.push_str(&media::update_fragment());
    html.push_str(&packages::update_fragment());
    html.push_str(&focus::oob_fragment());
    html.push_str(&weather::update_fragment());
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

    #[test]
    fn composes_all_ten_dock_components() {
        let dock = compose().unwrap();
        for element in [
            "clock-card",
            "controls-card",
            "bluetooth-card",
            "packages-card",
            "focus-card",
            "media-card",
            "system-card",
            "weather-card",
            "agent-card",
            "calendar-events-card",
        ] {
            assert!(dock.dashboard.contains(&format!(r#"id="{element}""#)));
        }
        assert!(dock.overlays.contains("clock-dialog"));
        assert!(dock.overlays.contains("media-dialog"));
    }
}
