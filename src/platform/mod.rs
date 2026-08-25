#[cfg(target_os = "windows")]
mod windows_tray;

#[cfg(target_os = "windows")]
pub use windows_tray::{run_tray, show_startup_error};

#[cfg(not(target_os = "windows"))]
pub fn run_tray() -> anyhow::Result<()> {
    anyhow::bail!("the Raylander tray is only available on Windows")
}

#[cfg(not(target_os = "windows"))]
pub fn show_startup_error(message: &str) {
    eprintln!("raylander-tray: {message}");
}
