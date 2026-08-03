//! Transport-independent state and action models for a StatusNotifier system tray.
//!
//! A D-Bus adapter owns discovery and property fetching. It turns those results
//! into [`TrayEvent`] values and applies them to [`TrayState`]. UI code can then
//! render [`TrayState::items`] and translate gestures into [`TrayAction`] values
//! without depending on a particular D-Bus implementation.

use std::collections::HashMap;

/// Stable identity assigned by the StatusNotifier protocol.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrayItemId {
    pub service: String,
    pub object_path: String,
}

impl TrayItemId {
    pub fn new(service: impl Into<String>, object_path: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            object_path: object_path.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ItemStatus {
    Passive,
    #[default]
    Active,
    NeedsAttention,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ItemCategory {
    ApplicationStatus,
    Communications,
    SystemServices,
    Hardware,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconPixmap {
    pub width: u32,
    pub height: u32,
    /// Pixels in the protocol's network-byte-order ARGB32 representation.
    pub argb: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Icon {
    pub name: Option<String>,
    pub pixmaps: Vec<IconPixmap>,
    pub theme_path: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolTip {
    pub icon: Icon,
    pub title: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleType {
    Checkmark,
    Radio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleState {
    Off,
    On,
    Indeterminate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItemKind {
    Standard,
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// The D-BusMenu item ID, opaque to the UI.
    pub id: i32,
    pub kind: MenuItemKind,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub icon: Icon,
    pub toggle: Option<(ToggleType, ToggleState)>,
    pub children: Vec<MenuItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    pub id: TrayItemId,
    pub title: String,
    pub status: ItemStatus,
    pub category: ItemCategory,
    pub icon: Icon,
    pub attention_icon: Icon,
    pub overlay_icon: Icon,
    pub tooltip: Option<ToolTip>,
    pub menu_path: Option<String>,
    pub menu: Vec<MenuItem>,
    pub item_is_menu: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAvailability {
    Starting,
    Available,
    Unavailable { reason: String },
}

impl Default for TrayAvailability {
    fn default() -> Self {
        Self::Starting
    }
}

/// Input emitted by a StatusNotifierWatcher/Host transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Availability(TrayAvailability),
    Registered(TrayItem),
    Updated(TrayItem),
    Removed(TrayItemId),
}

/// Observable result of applying an event. `Unchanged` allows render deduplication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayChange {
    Unchanged,
    AvailabilityChanged,
    ItemAdded,
    ItemUpdated,
    ItemRemoved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollOrientation {
    Horizontal,
    Vertical,
}

/// UI intent to be forwarded to the owning StatusNotifier item or D-BusMenu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Activate {
        item: TrayItemId,
        x: i32,
        y: i32,
    },
    SecondaryActivate {
        item: TrayItemId,
        x: i32,
        y: i32,
    },
    ContextMenu {
        item: TrayItemId,
        x: i32,
        y: i32,
    },
    Scroll {
        item: TrayItemId,
        delta: i32,
        orientation: ScrollOrientation,
    },
    MenuClicked {
        item: TrayItemId,
        menu_item: i32,
    },
    MenuHovered {
        item: TrayItemId,
        menu_item: i32,
    },
}

/// Ordered tray state. Existing registrations keep their original position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayState {
    availability: TrayAvailability,
    items: Vec<TrayItem>,
    positions: HashMap<TrayItemId, usize>,
}

impl TrayState {
    pub fn availability(&self) -> &TrayAvailability {
        &self.availability
    }

    pub fn items(&self) -> &[TrayItem] {
        &self.items
    }

    pub fn visible_items(&self) -> impl Iterator<Item = &TrayItem> {
        self.items
            .iter()
            .filter(|item| item.status != ItemStatus::Passive)
    }

    pub fn item(&self, id: &TrayItemId) -> Option<&TrayItem> {
        self.positions
            .get(id)
            .map(|position| &self.items[*position])
    }

    pub fn apply(&mut self, event: TrayEvent) -> TrayChange {
        match event {
            TrayEvent::Availability(availability) => {
                if self.availability == availability {
                    TrayChange::Unchanged
                } else {
                    self.availability = availability;
                    TrayChange::AvailabilityChanged
                }
            }
            TrayEvent::Registered(item) | TrayEvent::Updated(item) => self.upsert(item),
            TrayEvent::Removed(id) => self.remove(&id),
        }
    }

    fn upsert(&mut self, item: TrayItem) -> TrayChange {
        if let Some(position) = self.positions.get(&item.id).copied() {
            if self.items[position] == item {
                TrayChange::Unchanged
            } else {
                self.items[position] = item;
                TrayChange::ItemUpdated
            }
        } else {
            let position = self.items.len();
            self.positions.insert(item.id.clone(), position);
            self.items.push(item);
            TrayChange::ItemAdded
        }
    }

    fn remove(&mut self, id: &TrayItemId) -> TrayChange {
        let Some(position) = self.positions.remove(id) else {
            return TrayChange::Unchanged;
        };
        self.items.remove(position);
        for (offset, item) in self.items[position..].iter().enumerate() {
            self.positions.insert(item.id.clone(), position + offset);
        }
        TrayChange::ItemRemoved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(service: &str, title: &str, status: ItemStatus) -> TrayItem {
        TrayItem {
            id: TrayItemId::new(service, "/StatusNotifierItem"),
            title: title.into(),
            status,
            category: ItemCategory::ApplicationStatus,
            icon: Icon {
                name: Some("test-symbolic".into()),
                ..Icon::default()
            },
            attention_icon: Icon::default(),
            overlay_icon: Icon::default(),
            tooltip: None,
            menu_path: None,
            menu: Vec::new(),
            item_is_menu: false,
        }
    }

    #[test]
    fn registrations_are_ordered_and_duplicate_registration_updates_in_place() {
        let mut state = TrayState::default();
        assert_eq!(
            state.apply(TrayEvent::Registered(item("a", "A", ItemStatus::Active))),
            TrayChange::ItemAdded
        );
        state.apply(TrayEvent::Registered(item("b", "B", ItemStatus::Active)));
        assert_eq!(
            state.apply(TrayEvent::Registered(item(
                "a",
                "new A",
                ItemStatus::Active
            ))),
            TrayChange::ItemUpdated
        );
        assert_eq!(
            state
                .items()
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["new A", "B"]
        );
    }

    #[test]
    fn identical_updates_are_deduplicated() {
        let mut state = TrayState::default();
        let value = item("a", "A", ItemStatus::Active);
        state.apply(TrayEvent::Registered(value.clone()));
        assert_eq!(
            state.apply(TrayEvent::Updated(value)),
            TrayChange::Unchanged
        );
        assert_eq!(state.items().len(), 1);
    }

    #[test]
    fn update_before_registration_adds_item() {
        let mut state = TrayState::default();
        assert_eq!(
            state.apply(TrayEvent::Updated(item("a", "A", ItemStatus::Active))),
            TrayChange::ItemAdded
        );
    }

    #[test]
    fn removal_preserves_remaining_order_and_repairs_lookup_positions() {
        let mut state = TrayState::default();
        for service in ["a", "b", "c"] {
            state.apply(TrayEvent::Registered(item(
                service,
                service,
                ItemStatus::Active,
            )));
        }
        assert_eq!(
            state.apply(TrayEvent::Removed(TrayItemId::new(
                "b",
                "/StatusNotifierItem"
            ))),
            TrayChange::ItemRemoved
        );
        assert_eq!(
            state
                .items()
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(
            state
                .item(&TrayItemId::new("c", "/StatusNotifierItem"))
                .unwrap()
                .title,
            "c"
        );
        assert_eq!(
            state.apply(TrayEvent::Removed(TrayItemId::new(
                "missing",
                "/StatusNotifierItem"
            ))),
            TrayChange::Unchanged
        );
    }

    #[test]
    fn visibility_excludes_only_passive_items() {
        let mut state = TrayState::default();
        state.apply(TrayEvent::Registered(item(
            "passive",
            "P",
            ItemStatus::Passive,
        )));
        state.apply(TrayEvent::Registered(item(
            "active",
            "A",
            ItemStatus::Active,
        )));
        state.apply(TrayEvent::Registered(item(
            "attention",
            "N",
            ItemStatus::NeedsAttention,
        )));
        assert_eq!(
            state
                .visible_items()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["A", "N"]
        );
    }

    #[test]
    fn availability_changes_are_deduplicated_without_dropping_items() {
        let mut state = TrayState::default();
        state.apply(TrayEvent::Registered(item("a", "A", ItemStatus::Active)));
        assert_eq!(
            state.apply(TrayEvent::Availability(TrayAvailability::Available)),
            TrayChange::AvailabilityChanged
        );
        assert_eq!(
            state.apply(TrayEvent::Availability(TrayAvailability::Available)),
            TrayChange::Unchanged
        );
        assert_eq!(state.items().len(), 1);
        let unavailable = TrayAvailability::Unavailable {
            reason: "watcher disappeared".into(),
        };
        assert_eq!(
            state.apply(TrayEvent::Availability(unavailable.clone())),
            TrayChange::AvailabilityChanged
        );
        assert_eq!(state.availability(), &unavailable);
    }

    #[test]
    fn action_models_preserve_protocol_coordinates_and_menu_ids() {
        let id = TrayItemId::new("a", "/Item");
        assert_eq!(
            TrayAction::Scroll {
                item: id.clone(),
                delta: -120,
                orientation: ScrollOrientation::Vertical,
            },
            TrayAction::Scroll {
                item: id.clone(),
                delta: -120,
                orientation: ScrollOrientation::Vertical,
            }
        );
        assert_ne!(
            TrayAction::MenuClicked {
                item: id.clone(),
                menu_item: 4
            },
            TrayAction::MenuHovered {
                item: id,
                menu_item: 4
            }
        );
    }
}
