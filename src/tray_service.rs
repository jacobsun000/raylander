//! D-Bus transport for the transport-independent system tray model.
//!
//! The adapter deliberately uses polling for property refresh. Status notifier
//! implementations are inconsistent about which `New*` signals they emit, while
//! a short poll keeps the GTK side simple and also detects vanished bus names.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use zbus::{Connection, Proxy, message::Header, object_server::SignalEmitter};

use crate::tray::{
    Icon, IconPixmap, ItemCategory, ItemStatus, ScrollOrientation, ToolTip, TrayAction,
    TrayAvailability, TrayEvent, TrayItem, TrayItemId,
};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const WATCHER_IFACE: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";

enum Control {
    Action(TrayAction),
    Stop,
}

/// A running StatusNotifierHost. D-Bus work remains on its dedicated thread.
pub struct TrayService {
    control_tx: mpsc::Sender<Control>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TrayService {
    /// Connects to the session bus and returns a stream of typed tray events.
    ///
    /// Raylander registers with an existing watcher. If the watcher name is
    /// unowned, it publishes a minimal compatible watcher itself.
    pub fn start() -> Result<(Self, mpsc::Receiver<TrayEvent>)> {
        let (event_tx, event_rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("raylander-tray".into())
            .spawn(move || {
                if let Err(error) = zbus::block_on(run(event_tx, control_rx, ready_tx)) {
                    tracing::error!(%error, "tray D-Bus service stopped");
                }
            })
            .context("failed to spawn tray service thread")?;
        ready_rx
            .recv()
            .context("tray service exited during startup")??;
        Ok((
            Self {
                control_tx,
                thread: Some(thread),
            },
            event_rx,
        ))
    }

    /// Forwards a validated UI intent to the item that owns it.
    pub fn perform(&self, action: TrayAction) -> Result<()> {
        self.control_tx
            .send(Control::Action(action))
            .context("tray service is not running")
    }
}

impl Drop for TrayService {
    fn drop(&mut self) {
        let _ = self.control_tx.send(Control::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Default)]
struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    #[zbus(name = "RegisterStatusNotifierItem")]
    async fn register_item(
        &self,
        registration: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("missing D-Bus sender".into()))?;
        let id = normalize_registration(registration, sender.as_str())
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        let wire = wire_id(&id);
        let added = self
            .items
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("status notifier registry lock poisoned".into()))?
            .iter()
            .all(|item| item != &wire);
        if added {
            self.items.lock().unwrap().push(wire.clone());
            Self::item_registered(&emitter, &wire)
                .await
                .map_err(zbus::fdo::Error::ZBus)?;
        }
        Ok(())
    }

    #[zbus(name = "RegisterStatusNotifierHost")]
    fn register_host(&self, _service: &str) {}

    #[zbus(property, name = "RegisteredStatusNotifierItems")]
    fn registered_items(&self) -> Vec<String> {
        self.items
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    #[zbus(property, name = "IsStatusNotifierHostRegistered")]
    fn host_registered(&self) -> bool {
        true
    }

    #[zbus(property, name = "ProtocolVersion")]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal, name = "StatusNotifierItemRegistered")]
    async fn item_registered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;
}

async fn run(
    event_tx: mpsc::Sender<TrayEvent>,
    control_rx: mpsc::Receiver<Control>,
    ready_tx: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let connection = Connection::session()
        .await
        .context("failed to connect to session bus")?;
    let dbus = zbus::fdo::DBusProxy::new(&connection).await?;
    let watcher_name = WATCHER_NAME.try_into().expect("static bus name");
    let owns_watcher = !dbus.name_has_owner(watcher_name).await?;
    let local_items = if owns_watcher {
        let watcher = Watcher::default();
        let items = Arc::clone(&watcher.items);
        connection.object_server().at(WATCHER_PATH, watcher).await?;
        connection.request_name(WATCHER_NAME).await?;
        Some(items)
    } else {
        None
    };
    let watcher = Proxy::new(&connection, WATCHER_NAME, WATCHER_PATH, WATCHER_IFACE).await?;
    let host = connection
        .unique_name()
        .map(ToString::to_string)
        .unwrap_or_default();
    watcher
        .call::<_, _, ()>("RegisterStatusNotifierHost", &host)
        .await?;
    let _ = ready_tx.send(Ok(()));
    let _ = event_tx.send(TrayEvent::Availability(TrayAvailability::Available));

    let mut known = HashMap::<TrayItemId, TrayItem>::new();
    loop {
        match control_rx.recv_timeout(Duration::from_millis(750)) {
            Ok(Control::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Control::Action(action)) => {
                if let Err(error) = forward_action(&connection, action).await {
                    tracing::warn!(%error, "failed to forward tray action");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let registrations = if let Some(items) = &local_items {
            items.lock().map(|items| items.clone()).unwrap_or_default()
        } else {
            watcher
                .get_property::<Vec<String>>("RegisteredStatusNotifierItems")
                .await
                .unwrap_or_default()
        };
        let mut ids = HashSet::new();
        for value in &registrations {
            let Ok(id) = normalize_wire_id(value) else {
                tracing::warn!(
                    registration = value,
                    "watcher returned an invalid tray item"
                );
                continue;
            };
            let Ok(bus_name) = id.service.as_str().try_into() else {
                continue;
            };
            if dbus.name_has_owner(bus_name).await.unwrap_or(false) {
                ids.insert(id);
            }
        }
        if let Some(items) = &local_items
            && let Ok(mut registrations) = items.lock()
        {
            registrations
                .retain(|value| normalize_wire_id(value).is_ok_and(|id| ids.contains(&id)));
        }
        for removed in known
            .keys()
            .filter(|id| !ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>()
        {
            known.remove(&removed);
            let _ = event_tx.send(TrayEvent::Removed(removed));
        }
        for id in ids {
            match fetch_item(&connection, &id).await {
                Ok(item) => {
                    let event = if known.contains_key(&id) {
                        TrayEvent::Updated(item.clone())
                    } else {
                        TrayEvent::Registered(item.clone())
                    };
                    if known.get(&id) != Some(&item) {
                        known.insert(id, item);
                        let _ = event_tx.send(event);
                    }
                }
                Err(error) => tracing::debug!(%error, "could not refresh tray item"),
            }
        }
    }
    Ok(())
}

async fn fetch_item(connection: &Connection, id: &TrayItemId) -> Result<TrayItem> {
    let proxy = Proxy::new(
        connection,
        id.service.as_str(),
        id.object_path.as_str(),
        ITEM_IFACE,
    )
    .await?;
    let status = match property::<String>(&proxy, "Status").await.as_deref() {
        Some("Passive") => ItemStatus::Passive,
        Some("NeedsAttention") => ItemStatus::NeedsAttention,
        _ => ItemStatus::Active,
    };
    let category = match property::<String>(&proxy, "Category").await.as_deref() {
        Some("ApplicationStatus") => ItemCategory::ApplicationStatus,
        Some("Communications") => ItemCategory::Communications,
        Some("SystemServices") => ItemCategory::SystemServices,
        Some("Hardware") => ItemCategory::Hardware,
        _ => ItemCategory::Unknown,
    };
    let icon = Icon {
        name: property(&proxy, "IconName")
            .await
            .filter(|s: &String| !s.is_empty()),
        pixmaps: property::<Vec<(i32, i32, Vec<u8>)>>(&proxy, "IconPixmap")
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(pixmap)
            .collect(),
        theme_path: property(&proxy, "IconThemePath")
            .await
            .filter(|s: &String| !s.is_empty()),
    };
    let attention_icon = Icon {
        name: property(&proxy, "AttentionIconName")
            .await
            .filter(|s: &String| !s.is_empty()),
        pixmaps: property::<Vec<(i32, i32, Vec<u8>)>>(&proxy, "AttentionIconPixmap")
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(pixmap)
            .collect(),
        theme_path: None,
    };
    let overlay_icon = Icon {
        name: property(&proxy, "OverlayIconName")
            .await
            .filter(|s: &String| !s.is_empty()),
        pixmaps: property::<Vec<(i32, i32, Vec<u8>)>>(&proxy, "OverlayIconPixmap")
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(pixmap)
            .collect(),
        theme_path: None,
    };
    let tooltip = property::<(String, Vec<(i32, i32, Vec<u8>)>, String, String)>(&proxy, "ToolTip")
        .await
        .map(|(name, images, title, description)| ToolTip {
            icon: Icon {
                name: (!name.is_empty()).then_some(name),
                pixmaps: images.into_iter().filter_map(pixmap).collect(),
                theme_path: None,
            },
            title,
            description,
        });
    Ok(TrayItem {
        id: id.clone(),
        title: property(&proxy, "Title").await.unwrap_or_default(),
        status,
        category,
        icon,
        attention_icon,
        overlay_icon,
        tooltip,
        menu_path: property::<zbus::zvariant::OwnedObjectPath>(&proxy, "Menu")
            .await
            .map(|p| p.to_string()),
        menu: Vec::new(),
        item_is_menu: property(&proxy, "ItemIsMenu").await.unwrap_or(false),
    })
}

async fn property<T>(proxy: &Proxy<'_>, name: &str) -> Option<T>
where
    T: TryFrom<zbus::zvariant::OwnedValue>,
    T::Error: Into<zbus::Error>,
{
    proxy.get_property(name).await.ok()
}

async fn forward_action(connection: &Connection, action: TrayAction) -> Result<()> {
    let menu_action = match &action {
        TrayAction::MenuClicked { item, menu_item } => Some((item.clone(), *menu_item, "clicked")),
        TrayAction::MenuHovered { item, menu_item } => Some((item.clone(), *menu_item, "hovered")),
        _ => None,
    };
    if let Some((item, menu_item, event)) = menu_action {
        let item_proxy = Proxy::new(
            connection,
            item.service.clone(),
            item.object_path.clone(),
            ITEM_IFACE,
        )
        .await?;
        let menu_path: zbus::zvariant::OwnedObjectPath = item_proxy
            .get_property("Menu")
            .await
            .context("tray item does not expose a menu")?;
        let menu_proxy = Proxy::new(
            connection,
            item.service,
            menu_path,
            "com.canonical.dbusmenu",
        )
        .await?;
        menu_proxy
            .call::<_, _, ()>(
                "Event",
                &(
                    menu_item,
                    event,
                    zbus::zvariant::OwnedValue::from(0_i32),
                    0_u32,
                ),
            )
            .await?;
        return Ok(());
    }
    let (id, method, body): (&TrayItemId, &str, ActionBody) = match &action {
        TrayAction::Activate { item, x, y } => (item, "Activate", ActionBody::Point(*x, *y)),
        TrayAction::SecondaryActivate { item, x, y } => {
            (item, "SecondaryActivate", ActionBody::Point(*x, *y))
        }
        TrayAction::ContextMenu { item, x, y } => (item, "ContextMenu", ActionBody::Point(*x, *y)),
        TrayAction::Scroll {
            item,
            delta,
            orientation,
        } => (item, "Scroll", ActionBody::Scroll(*delta, *orientation)),
        TrayAction::MenuClicked { .. } | TrayAction::MenuHovered { .. } => unreachable!(),
    };
    let proxy = Proxy::new(
        connection,
        id.service.as_str(),
        id.object_path.as_str(),
        ITEM_IFACE,
    )
    .await?;
    match body {
        ActionBody::Point(x, y) => proxy.call::<_, _, ()>(method, &(x, y)).await?,
        ActionBody::Scroll(delta, orientation) => {
            let orientation = match orientation {
                ScrollOrientation::Horizontal => "horizontal",
                ScrollOrientation::Vertical => "vertical",
            };
            proxy
                .call::<_, _, ()>(method, &(delta, orientation))
                .await?;
        }
    }
    Ok(())
}

enum ActionBody {
    Point(i32, i32),
    Scroll(i32, ScrollOrientation),
}

fn pixmap((width, height, argb): (i32, i32, Vec<u8>)) -> Option<IconPixmap> {
    Some(IconPixmap {
        width: width.try_into().ok()?,
        height: height.try_into().ok()?,
        argb,
    })
}

fn normalize_registration(value: &str, sender: &str) -> Result<TrayItemId> {
    if value.starts_with('/') {
        validate_path(value)?;
        Ok(TrayItemId::new(sender, value))
    } else {
        validate_service(value)?;
        Ok(TrayItemId::new(value, DEFAULT_ITEM_PATH))
    }
}

fn normalize_wire_id(value: &str) -> Result<TrayItemId> {
    if let Some((service, path)) = value.split_once('/') {
        let path = format!("/{path}");
        validate_service(service)?;
        validate_path(&path)?;
        Ok(TrayItemId::new(service, path))
    } else {
        normalize_registration(value, value)
    }
}

fn wire_id(id: &TrayItemId) -> String {
    format!("{}{}", id.service, id.object_path)
}

fn validate_service(value: &str) -> Result<()> {
    let _: zbus::names::BusName<'_> = value.try_into().context("invalid service name")?;
    Ok(())
}

fn validate_path(value: &str) -> Result<()> {
    let _: zbus::zvariant::ObjectPath<'_> = value.try_into().context("invalid object path")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_registration_uses_callers_unique_name() {
        assert_eq!(
            normalize_registration("/Custom", ":1.42").unwrap(),
            TrayItemId::new(":1.42", "/Custom")
        );
    }

    #[test]
    fn service_registration_uses_default_path() {
        assert_eq!(
            normalize_registration("org.example.App", ":1.42").unwrap(),
            TrayItemId::new("org.example.App", DEFAULT_ITEM_PATH)
        );
    }

    #[test]
    fn watcher_wire_id_round_trips() {
        let id = TrayItemId::new(":1.77", "/StatusNotifierItem/one");
        assert_eq!(normalize_wire_id(&wire_id(&id)).unwrap(), id);
    }

    #[test]
    fn rejects_invalid_registration() {
        assert!(normalize_registration("not a service", ":1.2").is_err());
        assert!(normalize_registration("/not valid", ":1.2").is_err());
    }

    #[test]
    fn pixmap_rejects_negative_dimensions() {
        assert!(pixmap((-1, 12, vec![])).is_none());
    }
}
