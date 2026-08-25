//! Extensible dashboard component contracts and central layout management.
//!
//! Components describe what they need, while [`Layout`] decides where they
//! appear. In particular, components never know a slot, row, or column.

use std::{collections::HashSet, sync::Arc};

use anyhow::{Result, bail};
use axum::Router;

#[cfg(target_os = "linux")]
pub(crate) mod agents;
pub(crate) mod calendar_events;
pub(crate) mod clock;
pub(crate) mod connectivity;
pub(crate) mod controls;
pub mod dock;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) mod focus;
pub(crate) mod media;
pub(crate) mod native_observers;
#[cfg(target_os = "linux")]
pub(crate) mod packages;
pub(crate) mod system_status;
pub(crate) mod weather;

pub(crate) use dock::dock_component;

/// Stable, URL-safe identifier for a component.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ComponentId(&'static str);

impl ComponentId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }

    fn validate(self) -> Result<()> {
        if self.0.is_empty()
            || !self
                .0
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!(
                "component id {:?} must contain only lowercase ASCII letters, digits, and hyphens",
                self.0
            );
        }
        Ok(())
    }
}

/// Number of cells occupied by a component in the dashboard grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GridSize {
    pub columns: u16,
    pub rows: u16,
}

impl GridSize {
    pub const ONE_BY_ONE: Self = Self::new(1, 1);
    pub const ONE_BY_TWO: Self = Self::new(1, 2);
    pub const TWO_BY_ONE: Self = Self::new(2, 1);
    pub const TWO_BY_TWO: Self = Self::new(2, 2);

    pub const fn new(columns: u16, rows: u16) -> Self {
        assert!(columns > 0 && rows > 0, "component size must be non-zero");
        Self { columns, rows }
    }
}

/// A static web asset owned by a component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Asset {
    pub name: &'static str,
    pub content_type: &'static str,
    pub source: &'static [u8],
}

impl Asset {
    pub const fn new(
        name: &'static str,
        content_type: &'static str,
        source: &'static [u8],
    ) -> Self {
        Self {
            name,
            content_type,
            source,
        }
    }

    pub const fn stylesheet(name: &'static str, source: &'static str) -> Self {
        Self::new(name, "text/css; charset=utf-8", source.as_bytes())
    }

    pub const fn javascript(name: &'static str, source: &'static str) -> Self {
        Self::new(name, "text/javascript; charset=utf-8", source.as_bytes())
    }
}

/// HTML supplied by a component. Overlays are kept outside the grid so a
/// component can own dialogs without affecting its grid dimensions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentView {
    pub body: String,
    pub overlays: String,
}

/// Interface implemented by every dashboard component.
///
/// A small component can implement this entirely in its `mod.rs`. Larger
/// components may split their backend, routes, and rendering internally; that
/// organization is deliberately not part of this public contract.
pub trait Component: Send + Sync + 'static {
    fn id(&self) -> ComponentId;
    fn size(&self) -> GridSize;
    fn render(&self) -> Result<ComponentView>;

    /// Routes are mounted below `/components/<id>` by [`ComponentRegistry`].
    fn routes(&self) -> Router {
        Router::new()
    }

    fn assets(&self) -> &'static [Asset] {
        &[]
    }
}

/// Placement calculated by the central layout manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    pub component: ComponentId,
    pub column: u16,
    pub row: u16,
    pub size: GridSize,
}

/// Central dashboard layout policy. Component order belongs here, not in the
/// individual components.
#[derive(Clone, Debug)]
pub struct Layout {
    columns: u16,
    order: Vec<ComponentId>,
}

impl Layout {
    pub fn new(columns: u16, order: Vec<ComponentId>) -> Result<Self> {
        if columns == 0 {
            bail!("layout must have at least one column");
        }
        let mut seen = HashSet::new();
        if let Some(duplicate) = order.iter().find(|id| !seen.insert(**id)) {
            bail!(
                "component {} occurs more than once in layout",
                duplicate.as_str()
            );
        }
        Ok(Self { columns, order })
    }

    pub const fn columns(&self) -> u16 {
        self.columns
    }

    /// Uses deterministic first-fit packing. Components omitted from the
    /// explicit order follow registration order.
    pub fn arrange(&self, components: &[Arc<dyn Component>]) -> Result<Vec<Placement>> {
        let mut ordered = Vec::with_capacity(components.len());
        for id in &self.order {
            let component = components
                .iter()
                .find(|component| component.id() == *id)
                .ok_or_else(|| {
                    anyhow::anyhow!("layout references unknown component {}", id.as_str())
                })?;
            ordered.push(Arc::clone(component));
        }
        ordered.extend(
            components
                .iter()
                .filter(|component| !self.order.contains(&component.id()))
                .cloned(),
        );

        let mut occupied: Vec<Vec<bool>> = Vec::new();
        let mut placements = Vec::with_capacity(ordered.len());
        for component in ordered {
            let size = component.size();
            if size.columns > self.columns {
                bail!(
                    "component {} is {} columns wide but layout has {} columns",
                    component.id().as_str(),
                    size.columns,
                    self.columns
                );
            }
            let (column, row) = first_available(&mut occupied, self.columns, size);
            placements.push(Placement {
                component: component.id(),
                column,
                row,
                size,
            });
        }
        Ok(placements)
    }
}

fn first_available(occupied: &mut Vec<Vec<bool>>, columns: u16, size: GridSize) -> (u16, u16) {
    for row in 0.. {
        while occupied.len() < usize::from(row + size.rows) {
            occupied.push(vec![false; usize::from(columns)]);
        }
        for column in 0..=columns - size.columns {
            let free = (row..row + size.rows).all(|candidate_row| {
                (column..column + size.columns).all(|candidate_column| {
                    !occupied[usize::from(candidate_row)][usize::from(candidate_column)]
                })
            });
            if free {
                for occupied_row in row..row + size.rows {
                    for occupied_column in column..column + size.columns {
                        occupied[usize::from(occupied_row)][usize::from(occupied_column)] = true;
                    }
                }
                return (column, row);
            }
        }
    }
    unreachable!("an unbounded grid always has room")
}

/// Owns components and validates their shared namespace.
pub struct ComponentRegistry {
    components: Vec<Arc<dyn Component>>,
}

impl ComponentRegistry {
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    pub fn register(&mut self, component: Arc<dyn Component>) -> Result<()> {
        component.id().validate()?;
        if self
            .components
            .iter()
            .any(|registered| registered.id() == component.id())
        {
            bail!(
                "component {} is already registered",
                component.id().as_str()
            );
        }
        let mut names = HashSet::new();
        for asset in component.assets() {
            if asset.name.is_empty() || asset.name.contains('/') || asset.name.contains("..") {
                bail!(
                    "component {} has invalid asset name {:?}",
                    component.id().as_str(),
                    asset.name
                );
            }
            if !names.insert(asset.name) {
                bail!(
                    "component {} has duplicate asset {:?}",
                    component.id().as_str(),
                    asset.name
                );
            }
        }
        self.components.push(component);
        Ok(())
    }

    pub fn components(&self) -> &[Arc<dyn Component>] {
        &self.components
    }

    pub fn router(&self) -> Router {
        self.components
            .iter()
            .fold(Router::new(), |router, component| {
                router.nest(
                    &format!("/components/{}", component.id().as_str()),
                    component.routes(),
                )
            })
    }
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestComponent {
        id: ComponentId,
        size: GridSize,
    }

    impl Component for TestComponent {
        fn id(&self) -> ComponentId {
            self.id
        }

        fn size(&self) -> GridSize {
            self.size
        }

        fn render(&self) -> Result<ComponentView> {
            Ok(ComponentView::default())
        }
    }

    fn component(id: &'static str, size: GridSize) -> Arc<dyn Component> {
        Arc::new(TestComponent {
            id: ComponentId::new(id),
            size,
        })
    }

    #[test]
    fn layout_owns_order_and_packs_component_sizes() {
        let components = vec![
            component("clock", GridSize::ONE_BY_ONE),
            component("controls", GridSize::TWO_BY_ONE),
            component("media", GridSize::ONE_BY_TWO),
        ];
        let layout = Layout::new(
            2,
            vec![ComponentId::new("controls"), ComponentId::new("media")],
        )
        .unwrap();

        assert_eq!(
            layout.arrange(&components).unwrap(),
            vec![
                Placement {
                    component: ComponentId::new("controls"),
                    column: 0,
                    row: 0,
                    size: GridSize::TWO_BY_ONE,
                },
                Placement {
                    component: ComponentId::new("media"),
                    column: 0,
                    row: 1,
                    size: GridSize::ONE_BY_TWO,
                },
                Placement {
                    component: ComponentId::new("clock"),
                    column: 1,
                    row: 1,
                    size: GridSize::ONE_BY_ONE,
                },
            ]
        );
    }

    #[test]
    fn registry_rejects_duplicate_ids() {
        let mut registry = ComponentRegistry::new();
        registry
            .register(component("controls", GridSize::ONE_BY_ONE))
            .unwrap();
        assert!(
            registry
                .register(component("controls", GridSize::TWO_BY_ONE))
                .is_err()
        );
    }

    #[test]
    fn component_cannot_be_wider_than_layout() {
        let layout = Layout::new(1, Vec::new()).unwrap();
        assert!(
            layout
                .arrange(&[component("wide", GridSize::TWO_BY_ONE)])
                .is_err()
        );
    }
}
