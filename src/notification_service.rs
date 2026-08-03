//! `org.freedesktop.Notifications` adapter.
//!
//! The service owns its [`NotificationCenter`] on a dedicated thread. GTK code
//! consumes [`NotificationServiceEvent`] values and never performs D-Bus work.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use zbus::{object_server::SignalEmitter, zvariant::OwnedValue};

use crate::notifications::{
    ActionInvocation, CloseReason, ClosedNotification, ExpireTimeout, NotificationAction,
    NotificationCenter, NotificationHints, NotificationRequest, NotifyOutcome, Urgency,
};

const SERVICE_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationServiceEvent {
    Notified(NotifyOutcome),
    Closed(ClosedNotification),
    ActionInvoked(ActionInvocation),
}

enum Control {
    InvokeAction { id: u32, key: String },
    Close { id: u32, reason: CloseReason },
    SetDnd(bool),
    Expire { id: u32, received_at_ms: u64 },
    Stop,
}

/// Running notification daemon. Dropping it releases the well-known bus name.
pub struct NotificationService {
    control_tx: mpsc::Sender<Control>,
    thread: Option<thread::JoinHandle<()>>,
}

impl NotificationService {
    /// Starts the service and returns its transport-independent event stream.
    ///
    /// This fails when another notification daemon already owns the bus name.
    pub fn start(history_limit: usize) -> Result<(Self, mpsc::Receiver<NotificationServiceEvent>)> {
        let (event_tx, event_rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let service_control_tx = control_tx.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);

        let thread = thread::Builder::new()
            .name("raylander-notifications".into())
            .spawn(move || {
                let result = zbus::block_on(run_service(
                    history_limit,
                    event_tx,
                    control_rx,
                    service_control_tx,
                    ready_tx,
                ));
                if let Err(error) = result {
                    tracing::error!(%error, "notification D-Bus service stopped");
                }
            })
            .context("failed to spawn notification service thread")?;

        ready_rx
            .recv()
            .context("notification service exited during startup")??;
        Ok((
            Self {
                control_tx,
                thread: Some(thread),
            },
            event_rx,
        ))
    }

    /// Emits `ActionInvoked` after validating that the action is still active.
    pub fn invoke_action(&self, id: u32, key: impl Into<String>) -> Result<()> {
        self.control_tx
            .send(Control::InvokeAction {
                id,
                key: key.into(),
            })
            .context("notification service is not running")
    }

    /// Closes an active notification and emits `NotificationClosed`.
    pub fn close(&self, id: u32, reason: CloseReason) -> Result<()> {
        self.control_tx
            .send(Control::Close { id, reason })
            .context("notification service is not running")
    }

    pub fn set_dnd(&self, enabled: bool) -> Result<()> {
        self.control_tx
            .send(Control::SetDnd(enabled))
            .context("notification service is not running")
    }
}

impl Drop for NotificationService {
    fn drop(&mut self) {
        let _ = self.control_tx.send(Control::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct NotificationsInterface {
    center: Arc<Mutex<NotificationCenter>>,
    event_tx: mpsc::Sender<NotificationServiceEvent>,
    control_tx: mpsc::Sender<Control>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationsInterface {
    #[zbus(name = "GetCapabilities")]
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "persistence"]
    }

    #[zbus(name = "GetServerInformation")]
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Raylander", "Raylander", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "Notify")]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let request = make_request(
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            &hints,
            expire_timeout,
        )?;
        let outcome = self
            .center
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("notification state lock poisoned".into()))?
            .notify(request)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(format!("{error:?}")))?;
        let id = outcome.notification.id;
        if let Some(timeout) = expiration_duration(&outcome.notification) {
            let control_tx = self.control_tx.clone();
            let received_at_ms = outcome.notification.received_at_ms;
            thread::spawn(move || {
                thread::sleep(timeout);
                let _ = control_tx.send(Control::Expire { id, received_at_ms });
            });
        }
        let _ = self
            .event_tx
            .send(NotificationServiceEvent::Notified(outcome));
        Ok(id)
    }

    #[zbus(name = "CloseNotification")]
    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        if let Some(closed) = close_center(&self.center, id, CloseReason::ClosedByCall) {
            Self::notification_closed(&emitter, id, CloseReason::ClosedByCall as u32)
                .await
                .map_err(zbus::fdo::Error::ZBus)?;
            let _ = self.event_tx.send(NotificationServiceEvent::Closed(closed));
        }
        Ok(())
    }

    #[zbus(signal, name = "NotificationClosed")]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "ActionInvoked")]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

async fn run_service(
    history_limit: usize,
    event_tx: mpsc::Sender<NotificationServiceEvent>,
    control_rx: mpsc::Receiver<Control>,
    control_tx: mpsc::Sender<Control>,
    ready_tx: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let center = Arc::new(Mutex::new(NotificationCenter::new(history_limit)));
    let interface = NotificationsInterface {
        center: Arc::clone(&center),
        event_tx: event_tx.clone(),
        control_tx,
    };
    let connection = match zbus::connection::Builder::session()
        .context("failed to connect to the session bus")?
        .name(SERVICE_NAME)
        .context("invalid notification service name")?
        .serve_at(OBJECT_PATH, interface)
        .context("invalid notification object path")?
        .build()
        .await
        .context("failed to own org.freedesktop.Notifications")
    {
        Ok(connection) => {
            let _ = ready_tx.send(Ok(()));
            connection
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = ready_tx.send(Err(anyhow::anyhow!(message)));
            return Err(error);
        }
    };
    let emitter = SignalEmitter::new(&connection, OBJECT_PATH)?;

    loop {
        match control_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Control::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Control::Close { id, reason }) => {
                if let Some(closed) = close_center(&center, id, reason) {
                    NotificationsInterface::notification_closed(&emitter, id, reason as u32)
                        .await?;
                    let _ = event_tx.send(NotificationServiceEvent::Closed(closed));
                }
            }
            Ok(Control::SetDnd(enabled)) => {
                if let Ok(mut center) = center.lock() {
                    center.set_dnd(enabled);
                }
            }
            Ok(Control::Expire { id, received_at_ms }) => {
                let current_matches = center.lock().ok().is_some_and(|center| {
                    center.active().any(|notification| {
                        notification.id == id && notification.received_at_ms == received_at_ms
                    })
                });
                if current_matches
                    && let Some(closed) = close_center(&center, id, CloseReason::Expired)
                {
                    NotificationsInterface::notification_closed(
                        &emitter,
                        id,
                        CloseReason::Expired as u32,
                    )
                    .await?;
                    let _ = event_tx.send(NotificationServiceEvent::Closed(closed));
                }
            }
            Ok(Control::InvokeAction { id, key }) => {
                let invocation = center
                    .lock()
                    .ok()
                    .and_then(|center| center.invoke_action(id, &key).ok());
                if let Some(invocation) = invocation {
                    NotificationsInterface::action_invoked(&emitter, id, &key).await?;
                    let _ = event_tx.send(NotificationServiceEvent::ActionInvoked(invocation));
                }
            }
        }
    }
    Ok(())
}

fn expiration_duration(notification: &crate::notifications::Notification) -> Option<Duration> {
    match notification.timeout {
        ExpireTimeout::Never => None,
        ExpireTimeout::Milliseconds(value) => Some(Duration::from_millis(u64::from(value))),
        ExpireTimeout::Default => Some(Duration::from_millis(match notification.hints.urgency {
            Urgency::Low => 3_000,
            Urgency::Normal => 5_000,
            Urgency::Critical => 8_000,
        })),
    }
}

fn close_center(
    center: &Mutex<NotificationCenter>,
    id: u32,
    reason: CloseReason,
) -> Option<ClosedNotification> {
    center.lock().ok()?.close(id, reason)
}

#[allow(clippy::too_many_arguments)]
fn make_request(
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    action_pairs: Vec<String>,
    hints: &HashMap<String, OwnedValue>,
    expire_timeout: i32,
) -> zbus::fdo::Result<NotificationRequest> {
    let mut chunks = action_pairs.chunks_exact(2);
    let actions = chunks
        .by_ref()
        .map(|pair| NotificationAction {
            key: pair[0].clone(),
            label: pair[1].clone(),
        })
        .collect();
    if !chunks.remainder().is_empty() {
        return Err(zbus::fdo::Error::InvalidArgs(
            "actions must contain key/label pairs".into(),
        ));
    }
    Ok(NotificationRequest {
        app_name,
        replaces_id,
        app_icon,
        summary: strip_markup(&summary),
        body: strip_markup(&body),
        actions,
        hints: NotificationHints {
            urgency: hint_u8(hints, "urgency").map_or(Urgency::Normal, |value| match value {
                0 => Urgency::Low,
                2 => Urgency::Critical,
                _ => Urgency::Normal,
            }),
            transient: hint_bool(hints, "transient"),
            private: hint_bool(hints, "x-raylander-private"),
            category: hint_string(hints, "category"),
            desktop_entry: hint_string(hints, "desktop-entry"),
        },
        timeout: ExpireTimeout::from_freedesktop(expire_timeout),
        received_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    })
}

fn hint_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    hints.get(key).and_then(|value| u8::try_from(value).ok())
}

fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn hint_string(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    hints
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
}

fn strip_markup(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_notification_markup() {
        assert_eq!(
            strip_markup("Hello <b>small</b> world"),
            "Hello small world"
        );
    }

    #[test]
    fn rejects_unpaired_actions() {
        let error = make_request(
            "app".into(),
            0,
            String::new(),
            "summary".into(),
            String::new(),
            vec!["open".into()],
            &HashMap::new(),
            -1,
        );
        assert!(matches!(error, Err(zbus::fdo::Error::InvalidArgs(_))));
    }

    #[test]
    fn converts_action_pairs_and_timeout() {
        let request = make_request(
            "app".into(),
            7,
            String::new(),
            "summary".into(),
            "body".into(),
            vec!["open".into(), "Open".into()],
            &HashMap::new(),
            2500,
        )
        .unwrap();
        assert_eq!(request.actions[0].key, "open");
        assert_eq!(request.timeout, ExpireTimeout::Milliseconds(2500));
    }
}
