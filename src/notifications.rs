//! Transport-independent notification state and history.
//!
//! The D-Bus adapter is expected to translate freedesktop notification calls
//! into [`NotificationRequest`] values and render notifications only when
//! [`NotifyOutcome::should_present`] is true.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "milliseconds")]
pub enum ExpireTimeout {
    /// Let Raylander choose a timeout appropriate for the urgency.
    #[default]
    Default,
    Never,
    Milliseconds(u32),
}

impl ExpireTimeout {
    /// Converts the signed freedesktop timeout (`-1`, `0`, or milliseconds).
    pub fn from_freedesktop(value: i32) -> Self {
        match value {
            value if value < 0 => Self::Default,
            0 => Self::Never,
            value => Self::Milliseconds(value as u32),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationHints {
    pub urgency: Urgency,
    /// Transient notifications are displayed but never enter history.
    pub transient: bool,
    /// Private notifications are displayed but never enter persisted history.
    pub private: bool,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationRequest {
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub hints: NotificationHints,
    pub timeout: ExpireTimeout,
    /// Unix time supplied by the adapter, making this domain deterministic.
    pub received_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub hints: NotificationHints,
    pub timeout: ExpireTimeout,
    pub received_at_ms: u64,
}

impl Notification {
    fn from_request(id: u32, request: NotificationRequest) -> Self {
        Self {
            id,
            app_name: request.app_name,
            app_icon: request.app_icon,
            summary: request.summary,
            body: request.body,
            actions: request.actions,
            hints: request.hints,
            timeout: request.timeout,
            received_at_ms: request.received_at_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(u32)]
pub enum CloseReason {
    Expired = 1,
    DismissedByUser = 2,
    ClosedByCall = 3,
    Undefined = 4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotifyOutcome {
    pub notification: Notification,
    pub replaced: bool,
    pub should_present: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedNotification {
    pub notification: Notification,
    pub reason: CloseReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionInvocation {
    pub notification_id: u32,
    pub action_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationError {
    EmptyActionKey,
    DuplicateActionKey(String),
    UnknownNotification(u32),
    UnknownAction { notification_id: u32, key: String },
}

/// Owns active notifications and a newest-first, bounded history.
#[derive(Debug)]
pub struct NotificationCenter {
    active: BTreeMap<u32, Notification>,
    history: VecDeque<Notification>,
    history_limit: usize,
    next_id: u32,
    dnd: bool,
}

impl NotificationCenter {
    pub fn new(history_limit: usize) -> Self {
        Self {
            active: BTreeMap::new(),
            history: VecDeque::new(),
            history_limit,
            next_id: 1,
            dnd: false,
        }
    }

    pub fn dnd(&self) -> bool {
        self.dnd
    }

    pub fn set_dnd(&mut self, enabled: bool) {
        self.dnd = enabled;
    }

    pub fn active(&self) -> impl Iterator<Item = &Notification> {
        self.active.values()
    }

    pub fn history(&self) -> impl Iterator<Item = &Notification> {
        self.history.iter()
    }

    pub fn notify(
        &mut self,
        request: NotificationRequest,
    ) -> Result<NotifyOutcome, NotificationError> {
        validate_actions(&request.actions)?;
        let replaced = request.replaces_id != 0 && self.active.contains_key(&request.replaces_id);
        let id = if replaced {
            request.replaces_id
        } else {
            self.allocate_id()
        };
        let notification = Notification::from_request(id, request);
        self.active.insert(id, notification.clone());
        self.record_history(&notification);
        Ok(NotifyOutcome {
            notification,
            replaced,
            should_present: !self.dnd,
        })
    }

    pub fn close(&mut self, id: u32, reason: CloseReason) -> Option<ClosedNotification> {
        self.active
            .remove(&id)
            .map(|notification| ClosedNotification {
                notification,
                reason,
            })
    }

    pub fn invoke_action(
        &self,
        notification_id: u32,
        key: &str,
    ) -> Result<ActionInvocation, NotificationError> {
        let notification = self
            .active
            .get(&notification_id)
            .ok_or(NotificationError::UnknownNotification(notification_id))?;
        if !notification.actions.iter().any(|action| action.key == key) {
            return Err(NotificationError::UnknownAction {
                notification_id,
                key: key.to_owned(),
            });
        }
        Ok(ActionInvocation {
            notification_id,
            action_key: key.to_owned(),
        })
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Replaces persisted history, filtering entries that must not be retained.
    pub fn restore_history(&mut self, notifications: Vec<Notification>) {
        self.history = notifications
            .into_iter()
            .filter(|notification| !notification.hints.transient && !notification.hints.private)
            .take(self.history_limit)
            .collect();
    }

    fn record_history(&mut self, notification: &Notification) {
        if self.history_limit == 0 || notification.hints.transient || notification.hints.private {
            return;
        }
        // A replacement updates the existing history item instead of duplicating it.
        self.history.retain(|entry| entry.id != notification.id);
        self.history.push_front(notification.clone());
        self.history.truncate(self.history_limit);
    }

    fn allocate_id(&mut self) -> u32 {
        loop {
            let candidate = self.next_id.max(1);
            self.next_id = candidate.wrapping_add(1).max(1);
            if !self.active.contains_key(&candidate) {
                return candidate;
            }
        }
    }
}

fn validate_actions(actions: &[NotificationAction]) -> Result<(), NotificationError> {
    let mut keys = HashSet::with_capacity(actions.len());
    for action in actions {
        if action.key.is_empty() {
            return Err(NotificationError::EmptyActionKey);
        }
        if !keys.insert(&action.key) {
            return Err(NotificationError::DuplicateActionKey(action.key.clone()));
        }
    }
    Ok(())
}

/// Atomically persists already-encoded history bytes beside the destination.
///
/// The integration layer should encode `center.history().cloned().collect()`
/// with `serde_json`, then call this function. Keeping encoding outside this
/// module lets the state engine compile before that dependency is introduced.
pub fn persist_atomically(path: &Path, encoded: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("notifications.json");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(encoded)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(summary: &str) -> NotificationRequest {
        NotificationRequest {
            app_name: "Test".into(),
            replaces_id: 0,
            app_icon: String::new(),
            summary: summary.into(),
            body: "body".into(),
            actions: vec![NotificationAction {
                key: "open".into(),
                label: "Open".into(),
            }],
            hints: NotificationHints::default(),
            timeout: ExpireTimeout::Default,
            received_at_ms: 42,
        }
    }

    #[test]
    fn converts_freedesktop_timeouts() {
        assert_eq!(ExpireTimeout::from_freedesktop(-1), ExpireTimeout::Default);
        assert_eq!(ExpireTimeout::from_freedesktop(0), ExpireTimeout::Never);
        assert_eq!(
            ExpireTimeout::from_freedesktop(900),
            ExpireTimeout::Milliseconds(900)
        );
    }

    #[test]
    fn replacement_reuses_id_and_history_position() {
        let mut center = NotificationCenter::new(100);
        let first = center.notify(request("old")).unwrap().notification.id;
        let mut replacement = request("new");
        replacement.replaces_id = first;
        let outcome = center.notify(replacement).unwrap();
        assert!(outcome.replaced);
        assert_eq!(outcome.notification.id, first);
        assert_eq!(center.active().count(), 1);
        assert_eq!(center.history().count(), 1);
        assert_eq!(center.history().next().unwrap().summary, "new");
    }

    #[test]
    fn missing_replacement_id_allocates_a_new_id() {
        let mut center = NotificationCenter::new(10);
        let mut value = request("new");
        value.replaces_id = 999;
        let outcome = center.notify(value).unwrap();
        assert!(!outcome.replaced);
        assert_ne!(outcome.notification.id, 999);
    }

    #[test]
    fn dnd_accepts_and_records_without_presenting() {
        let mut center = NotificationCenter::new(10);
        center.set_dnd(true);
        let outcome = center.notify(request("quiet")).unwrap();
        assert!(!outcome.should_present);
        assert_eq!(center.active().count(), 1);
        assert_eq!(center.history().count(), 1);
    }

    #[test]
    fn transient_and_private_notifications_are_not_retained() {
        let mut center = NotificationCenter::new(10);
        let mut transient = request("transient");
        transient.hints.transient = true;
        center.notify(transient).unwrap();
        let mut private = request("private");
        private.hints.private = true;
        center.notify(private).unwrap();
        assert_eq!(center.active().count(), 2);
        assert_eq!(center.history().count(), 0);
    }

    #[test]
    fn history_is_newest_first_and_bounded() {
        let mut center = NotificationCenter::new(2);
        center.notify(request("one")).unwrap();
        center.notify(request("two")).unwrap();
        center.notify(request("three")).unwrap();
        let summaries: Vec<_> = center.history().map(|item| item.summary.as_str()).collect();
        assert_eq!(summaries, ["three", "two"]);
    }

    #[test]
    fn validates_action_keys_and_invocations() {
        let mut center = NotificationCenter::new(10);
        let id = center.notify(request("action")).unwrap().notification.id;
        assert_eq!(center.invoke_action(id, "open").unwrap().action_key, "open");
        assert!(matches!(
            center.invoke_action(id, "missing"),
            Err(NotificationError::UnknownAction { .. })
        ));
        let mut duplicate = request("bad");
        duplicate.actions.push(duplicate.actions[0].clone());
        assert!(matches!(
            center.notify(duplicate),
            Err(NotificationError::DuplicateActionKey(_))
        ));
    }

    #[test]
    fn close_preserves_freedesktop_reason() {
        let mut center = NotificationCenter::new(10);
        let id = center.notify(request("close")).unwrap().notification.id;
        let closed = center.close(id, CloseReason::ClosedByCall).unwrap();
        assert_eq!(closed.reason as u32, 3);
        assert_eq!(center.active().count(), 0);
    }

    #[test]
    fn restore_filters_sensitive_entries_and_applies_limit() {
        let mut normal = Notification::from_request(1, request("one"));
        normal.id = 1;
        let mut private = normal.clone();
        private.id = 2;
        private.hints.private = true;
        let mut second = normal.clone();
        second.id = 3;
        let mut center = NotificationCenter::new(1);
        center.restore_history(vec![private, normal, second]);
        assert_eq!(
            center.history().map(|item| item.id).collect::<Vec<_>>(),
            [1]
        );
    }

    #[test]
    fn atomic_persistence_replaces_destination() {
        let directory = std::env::temp_dir().join(format!(
            "raylander-notification-test-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = directory.join("history.json");
        persist_atomically(&path, b"old").unwrap();
        persist_atomically(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(directory).unwrap();
    }
}
