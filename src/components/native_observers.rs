//! Platform-native change notifications used by component observers.

#[cfg(target_os = "linux")]
#[path = "native_observers/linux.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "native_observers/windows.rs"]
mod platform;

pub use platform::{observe_media, observe_volume};
