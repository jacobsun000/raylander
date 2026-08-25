mod app;
pub mod codex_hooks;
pub mod components;
mod config;
mod platform;
mod web_server;

pub use app::run;
pub use platform::{run_tray, show_startup_error as show_tray_startup_error};
